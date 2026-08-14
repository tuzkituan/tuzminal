//! The Lua plugin runtime.
//!
//! A plugin is a Lua file returning a table of handlers:
//!
//! ```lua
//! local M = {}
//! function M.on_startup(ctx)
//!   ctx.register_command("toggle", "Toggle the thing")
//!   ctx.register_keybind("ctrl+shift+b", "toggle")
//! end
//! function M.on_key(ctx, key)
//!   if key.chord == "ctrl+shift+q" then return true end  -- claim the key
//! end
//! return M
//! ```
//!
//! # Sandboxing is best-effort, not a boundary
//!
//! The environment is restricted — `io`, `os.execute`, `require`, `dofile` and
//! `load` are removed unless the manifest grants the matching permission — which
//! stops accidents and makes intent explicit. It is **not** a security boundary: a
//! determined plugin can reach the C API through paths this cannot close. The host
//! says so out loud at load time, and users installing a Lua plugin are trusting
//! its code. Real isolation is what the WASM runtime is for.
//!
//! # Timeouts
//!
//! An instruction-count hook aborts a callback that runs too long, so an infinite
//! loop in a plugin costs one aborted call rather than a frozen terminal.

use crate::{PluginError, PluginRuntime};
use mlua::{Lua, Table, Value, Variadic};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use tuz_plugin_api::{
    Command, Direction, Event, KeyOutcome, Manifest, NotifyLevel, PaneId, Permission, StatusSegment,
};

/// Instructions between timeout checks. Small enough to abort promptly, large
/// enough that the hook itself is not the bottleneck.
const HOOK_INTERVAL: u32 = 10_000;

/// Commands a callback queued, shared between Rust and the Lua context table.
type CommandSink = Rc<RefCell<Vec<Command>>>;

pub struct LuaPlugin {
    lua: Lua,
    name: String,
    /// The table the plugin's entry file returned.
    handlers: mlua::RegistryKey,
    commands: CommandSink,
    timeout: Duration,
}

impl LuaPlugin {
    /// Load and evaluate a plugin's entry file.
    pub fn load(manifest: &Manifest, entry: &Path, timeout: Duration) -> Result<Self, PluginError> {
        let source = std::fs::read_to_string(entry).map_err(|source| PluginError::Io {
            path: entry.to_owned(),
            source,
        })?;

        let lua = Lua::new();
        let commands: CommandSink = Rc::new(RefCell::new(Vec::new()));

        restrict_environment(&lua, manifest).map_err(|e| PluginError::Init {
            plugin: manifest.name.clone(),
            message: format!("failed to set up the sandbox: {e}"),
        })?;

        install_timeout(&lua, timeout);

        let chunk_name = format!("@{}", entry.display());
        let returned: Value =
            lua.load(&source)
                .set_name(chunk_name)
                .eval()
                .map_err(|e| PluginError::Init {
                    plugin: manifest.name.clone(),
                    message: e.to_string(),
                })?;

        let handlers = match returned {
            Value::Table(table) => table,
            // A plugin that forgot `return M` is a common mistake worth naming
            // precisely rather than failing later with "attempt to index nil".
            other => {
                return Err(PluginError::Init {
                    plugin: manifest.name.clone(),
                    message: format!(
                        "the entry file must return a table of handlers, got {}; \
                         did you forget `return M`?",
                        other.type_name()
                    ),
                })
            }
        };

        let handlers = lua
            .create_registry_value(handlers)
            .map_err(|e| PluginError::Init {
                plugin: manifest.name.clone(),
                message: e.to_string(),
            })?;

        Ok(Self {
            lua,
            name: manifest.name.clone(),
            handlers,
            commands,
            timeout,
        })
    }

    /// Build the `ctx` table handed to every callback.
    ///
    /// Each function pushes a [`Command`] onto the sink rather than acting
    /// directly, which is what keeps plugins unable to touch terminal state.
    fn context(&self) -> Result<Table, mlua::Error> {
        let ctx = self.lua.create_table()?;
        let sink = self.commands.clone();

        // Each entry turns Lua arguments into a Command and queues it. The `fn`
        // annotation pins the closure's error type, which inference cannot
        // otherwise pick out of mlua's many `From` impls.
        macro_rules! push_command {
            ($name:literal, $build:expr) => {{
                let sink = sink.clone();
                let build: fn(Variadic<Value>) -> mlua::Result<Command> = $build;
                let f = self.lua.create_function(move |_, args: Variadic<Value>| {
                    let command = build(args)?;
                    sink.borrow_mut().push(command);
                    Ok(())
                })?;
                ctx.set($name, f)?;
            }};
        }

        push_command!("new_tab", |_args: Variadic<Value>| Ok(Command::NewTab));
        push_command!("quit", |_args: Variadic<Value>| Ok(Command::Quit));
        push_command!("reload_config", |_args: Variadic<Value>| Ok(
            Command::ReloadConfig
        ));

        push_command!("split", |args: Variadic<Value>| {
            let direction = direction_from(args.first())?;
            Ok(Command::Split { direction })
        });
        push_command!("focus", |args: Variadic<Value>| {
            let direction = direction_from(args.first())?;
            Ok(Command::Focus { direction })
        });
        push_command!("close_pane", |_args: Variadic<Value>| Ok(
            Command::ClosePane { pane: None }
        ));

        push_command!("send_text", |args: Variadic<Value>| {
            let text = string_arg(args.first(), "send_text")?;
            Ok(Command::SendText { pane: None, text })
        });

        push_command!("notify", |args: Variadic<Value>| {
            let message = string_arg(args.first(), "notify")?;
            let level = match args.get(1).and_then(|v| {
                v.as_string()
                    .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
            }) {
                Some(s) if s == "warn" => NotifyLevel::Warn,
                Some(s) if s == "error" => NotifyLevel::Error,
                _ => NotifyLevel::Info,
            };
            Ok(Command::Notify { message, level })
        });

        push_command!("register_command", |args: Variadic<Value>| {
            let name = string_arg(args.first(), "register_command")?;
            let description = args
                .get(1)
                .and_then(|v| {
                    v.as_string()
                        .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
                })
                .unwrap_or_default();
            Ok(Command::RegisterCommand { name, description })
        });

        push_command!("register_keybind", |args: Variadic<Value>| {
            let chord = string_arg(args.first(), "register_keybind")?;
            let command = string_arg(args.get(1), "register_keybind")?;
            Ok(Command::RegisterKeybind { chord, command })
        });

        // The five commands a Lua plugin could not previously emit at all. WASM
        // plugins serialize `Command` directly, so they always could — the two
        // runtimes are meant to have the same reach, and this closes the gap.

        push_command!("focus_pane", |args: Variadic<Value>| {
            let pane = pane_arg(args.first(), "focus_pane")?;
            Ok(Command::FocusPane { pane })
        });

        push_command!("close_pane_id", |args: Variadic<Value>| {
            // Named apart from `close_pane`, which takes no argument and means the
            // focused one. Overloading on arity would make a typo silently close the
            // wrong pane.
            let pane = pane_arg(args.first(), "close_pane_id")?;
            Ok(Command::ClosePane { pane: Some(pane) })
        });

        push_command!("send_text_to", |args: Variadic<Value>| {
            let pane = pane_arg(args.first(), "send_text_to")?;
            let text = string_arg(args.get(1), "send_text_to")?;
            Ok(Command::SendText {
                pane: Some(pane),
                text,
            })
        });

        push_command!("resize", |args: Variadic<Value>| {
            let direction = direction_from(args.first())?;
            let delta = args
                .get(1)
                .and_then(|v| v.as_f32())
                .ok_or_else(|| mlua::Error::runtime("resize expects a number"))?;
            Ok(Command::Resize { direction, delta })
        });

        push_command!("set_config", |args: Variadic<Value>| {
            let toml = string_arg(args.first(), "set_config")?;
            Ok(Command::SetConfigOverlay { toml })
        });

        push_command!("select_tab", |args: Variadic<Value>| {
            let index = args
                .first()
                .and_then(|v| v.as_integer())
                .ok_or_else(|| mlua::Error::runtime("select_tab expects a number"))?;
            Ok(Command::SelectTab {
                index: index.max(0) as u32,
            })
        });

        // Status segments take a list of tables, so it does not fit the macro.
        {
            let sink = sink.clone();
            let f = self.lua.create_function(move |_, segments: Vec<Table>| {
                let mut out = Vec::with_capacity(segments.len());
                for segment in segments {
                    out.push(StatusSegment {
                        text: segment.get::<String>("text").unwrap_or_default(),
                        // An `id` makes the segment clickable; without one it is
                        // drawn and ignored.
                        id: segment.get::<Option<String>>("id").ok().flatten(),
                        foreground: segment.get::<Option<String>>("foreground").ok().flatten(),
                        background: segment.get::<Option<String>>("background").ok().flatten(),
                    });
                }
                sink.borrow_mut()
                    .push(Command::SetStatusSegments { segments: out });
                Ok(())
            })?;
            ctx.set("set_status", f)?;
        }

        // `log` is the one function that does something immediately: a plugin
        // debugging itself should not have its output queued behind commands.
        {
            let name = self.name.clone();
            let f = self.lua.create_function(move |_, message: String| {
                log::info!("[{name}] {message}");
                Ok(())
            })?;
            ctx.set("log", f)?;
        }

        Ok(ctx)
    }

    /// Call a handler by name, returning its raw Lua result.
    fn call_handler(
        &mut self,
        handler: &str,
        extra: Option<Table>,
    ) -> Result<Option<Value>, PluginError> {
        let handlers: Table =
            self.lua
                .registry_value(&self.handlers)
                .map_err(|e| PluginError::Runtime {
                    plugin: self.name.clone(),
                    message: e.to_string(),
                })?;

        let function: Value = handlers.get(handler).map_err(|e| PluginError::Runtime {
            plugin: self.name.clone(),
            message: e.to_string(),
        })?;

        // A plugin that does not implement a handler is normal, not an error.
        let Value::Function(function) = function else {
            return Ok(None);
        };

        let ctx = self.context().map_err(|e| PluginError::Runtime {
            plugin: self.name.clone(),
            message: e.to_string(),
        })?;

        // Reset the deadline so each call gets the full budget rather than
        // inheriting the elapsed time of previous ones.
        install_timeout(&self.lua, self.timeout);

        let result: Value = match extra {
            Some(payload) => function.call((ctx, payload)),
            None => function.call(ctx),
        }
        .map_err(|e| classify(&self.name, e))?;

        Ok(Some(result))
    }

    fn take_commands(&self) -> Vec<Command> {
        std::mem::take(&mut *self.commands.borrow_mut())
    }
}

impl PluginRuntime for LuaPlugin {
    fn dispatch(&mut self, event: &Event) -> Result<Vec<Command>, PluginError> {
        let (handler, payload) = match event {
            Event::Startup => ("on_startup", None),
            Event::ConfigReload => ("on_config_reload", None),
            Event::StatusBarRender => ("on_status_bar_render", None),
            Event::StatusSegmentClick { id } => {
                let t = self.table()?;
                let _ = t.set("id", id.clone());
                ("on_status_segment_click", Some(t))
            }
            Event::Bell { pane } => ("on_bell", Some(self.pane_table(*pane)?)),
            Event::PaneOpened { pane } => ("on_pane_opened", Some(self.pane_table(*pane)?)),
            Event::PaneClosed { pane } => ("on_pane_closed", Some(self.pane_table(*pane)?)),
            Event::TabSwitch { index } => {
                let t = self.table()?;
                let _ = t.set("index", *index);
                ("on_tab_switch", Some(t))
            }
            Event::TitleChange { pane, title } => {
                let t = self.pane_table(*pane)?;
                let _ = t.set("title", title.clone());
                ("on_title_change", Some(t))
            }
            Event::PaneOutput { pane, text } => {
                let t = self.pane_table(*pane)?;
                let _ = t.set("text", text.clone());
                ("on_pane_output", Some(t))
            }
            Event::Osc {
                pane,
                code,
                payload,
            } => {
                let t = self.pane_table(*pane)?;
                let _ = t.set("code", *code);
                let _ = t.set("payload", payload.clone());
                ("on_osc", Some(t))
            }
            Event::Command { name, args } => {
                let t = self.table()?;
                let _ = t.set("name", name.clone());
                let _ = t.set("args", args.clone());
                ("on_command", Some(t))
            }
            // `on_key` goes through `on_key`, not here.
            Event::Key(_) => return Ok(Vec::new()),
            _ => return Ok(Vec::new()),
        };

        self.call_handler(handler, payload)?;
        Ok(self.take_commands())
    }

    fn on_key(&mut self, event: &Event) -> Result<(KeyOutcome, Vec<Command>), PluginError> {
        let Event::Key(key) = event else {
            return Ok((KeyOutcome::Unhandled, Vec::new()));
        };

        let payload = self.table()?;
        let _ = payload.set("chord", key.chord.clone());
        let _ = payload.set("ctrl", key.modifiers.ctrl);
        let _ = payload.set("shift", key.modifiers.shift);
        let _ = payload.set("alt", key.modifiers.alt);
        let _ = payload.set("super", key.modifiers.super_key);

        let result = self.call_handler("on_key", Some(payload))?;

        // Only an explicit `true` claims the key. A handler that returns nothing
        // must not swallow input, which is why `nil` maps to Unhandled.
        let outcome = match result {
            Some(Value::Boolean(true)) => KeyOutcome::Handled,
            _ => KeyOutcome::Unhandled,
        };
        Ok((outcome, self.take_commands()))
    }

    fn runtime_name(&self) -> &'static str {
        "lua"
    }
}

impl LuaPlugin {
    /// A fresh Lua table, with allocation failure mapped to our error type.
    fn table(&self) -> Result<Table, PluginError> {
        self.lua.create_table().map_err(|e| PluginError::Runtime {
            plugin: self.name.clone(),
            message: e.to_string(),
        })
    }

    fn pane_table(&self, pane: PaneId) -> Result<Table, PluginError> {
        let t = self.table()?;
        let _ = t.set("pane", pane.0);
        Ok(t)
    }
}

/// Map an mlua error onto our error type, separating timeouts from real faults.
fn classify(plugin: &str, error: mlua::Error) -> PluginError {
    let text = error.to_string();
    if text.contains("execution budget") {
        return PluginError::Timeout {
            plugin: plugin.to_owned(),
        };
    }
    PluginError::Runtime {
        plugin: plugin.to_owned(),
        message: text,
    }
}

/// Abort a callback that runs longer than `timeout`.
///
/// An instruction-count hook rather than a wall-clock thread: it needs no extra
/// thread and cannot interrupt the VM at an unsafe point.
fn install_timeout(lua: &Lua, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    // Failure here means the VM refused the hook; the callback then runs without
    // a deadline, which is worth a warning but not worth refusing to load.
    let installed = lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
        move |_lua, _debug| {
            if std::time::Instant::now() > deadline {
                // The message is matched by `classify`, so keep them in step.
                return Err(mlua::Error::runtime("plugin exceeded its execution budget"));
            }
            Ok(mlua::VmState::Continue)
        },
    );
    if let Err(e) = installed {
        log::warn!("could not install the plugin timeout hook: {e}");
    }
}

/// Remove dangerous globals unless the manifest grants the matching permission.
///
/// Best-effort by design: see the module docs. The value is that a plugin which
/// never declared `spawn-process` cannot call `os.execute` *by accident*, and that
/// the manifest states intent a reviewer can read.
fn restrict_environment(lua: &Lua, manifest: &Manifest) -> Result<(), mlua::Error> {
    let globals = lua.globals();

    let may_spawn = manifest.has_permission(&Permission::SpawnProcess);
    let may_read = manifest
        .permissions
        .iter()
        .any(|p| matches!(p, Permission::FsRead(_)));
    let may_write = manifest
        .permissions
        .iter()
        .any(|p| matches!(p, Permission::FsWrite(_)));

    // `io` is all filesystem access; drop it entirely unless some file permission
    // was requested.
    if !may_read && !may_write {
        globals.set("io", Value::Nil)?;
    }

    if let Ok(os) = globals.get::<Table>("os") {
        if !may_spawn {
            os.set("execute", Value::Nil)?;
            os.set("tmpname", Value::Nil)?;
        }
        if !may_write {
            os.set("remove", Value::Nil)?;
            os.set("rename", Value::Nil)?;
        }
        // Never available: a plugin exiting the process would look like a terminal
        // crash to the user.
        os.set("exit", Value::Nil)?;
    }

    // Loading more code sidesteps every check above, so these always go.
    globals.set("dofile", Value::Nil)?;
    globals.set("loadfile", Value::Nil)?;
    globals.set("load", Value::Nil)?;
    globals.set("require", Value::Nil)?;

    // `debug` can reach removed functions through upvalues and registry access.
    globals.set("debug", Value::Nil)?;

    Ok(())
}

fn direction_from(value: Option<&Value>) -> Result<Direction, mlua::Error> {
    let text = value
        .and_then(|v| {
            v.as_string()
                .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
        })
        .ok_or_else(|| {
            mlua::Error::runtime("expected a direction: \"left\", \"right\", \"up\" or \"down\"")
        })?;
    Ok(match text.as_str() {
        "left" => Direction::Left,
        "right" => Direction::Right,
        "up" => Direction::Up,
        "down" => Direction::Down,
        other => {
            return Err(mlua::Error::runtime(format!(
                "unknown direction `{other}`; expected left, right, up or down"
            )))
        }
    })
}

/// A pane id argument, as a plain integer.
///
/// Lua has one number type, so the id arrives as an integer rather than the `pane1`
/// string form `Display` produces; a negative one is a mistake worth reporting rather
/// than wrapping into a very large pane that does not exist.
fn pane_arg(value: Option<&Value>, function: &str) -> Result<PaneId, mlua::Error> {
    let n = value
        .and_then(|v| v.as_integer())
        .ok_or_else(|| mlua::Error::runtime(format!("{function} expects a pane id")))?;
    if n < 0 {
        return Err(mlua::Error::runtime(format!(
            "{function} got a negative pane id"
        )));
    }
    Ok(PaneId(n as u32))
}

fn string_arg(value: Option<&Value>, function: &str) -> Result<String, mlua::Error> {
    value
        .and_then(|v| {
            v.as_string()
                .and_then(|s| s.to_str().ok().map(|s| s.to_string()))
        })
        .ok_or_else(|| mlua::Error::runtime(format!("{function} expects a string")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_plugin_api::{KeyPress, Runtime};

    fn manifest(permissions: Vec<Permission>) -> Manifest {
        Manifest {
            name: "test".to_owned(),
            version: "0.1.0".to_owned(),
            api_version: tuz_plugin_api::API_VERSION,
            runtime: Runtime::Lua,
            entry: "init.lua".to_owned(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            permissions,
            events: Vec::new(),
            config: Default::default(),
        }
    }

    /// Unwrap the error from a failed load. `LuaPlugin` is not `Debug`, so
    /// `unwrap_err` cannot be used directly.
    fn expect_load_error(result: Result<LuaPlugin, PluginError>) -> PluginError {
        match result {
            Ok(_) => panic!("expected the load to fail"),
            Err(e) => e,
        }
    }

    /// Load a plugin from inline source.
    fn plugin(source: &str) -> Result<LuaPlugin, PluginError> {
        plugin_with(source, vec![])
    }

    fn plugin_with(source: &str, permissions: Vec<Permission>) -> Result<LuaPlugin, PluginError> {
        let dir = std::env::temp_dir().join(format!(
            "tuz-lua-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("init.lua");
        std::fs::write(&path, source).unwrap();
        LuaPlugin::load(&manifest(permissions), &path, Duration::from_millis(500))
    }

    #[test]
    fn a_plugin_that_returns_a_table_loads() {
        let p = plugin("local M = {}\nreturn M\n").expect("should load");
        assert_eq!(p.runtime_name(), "lua");
    }

    #[test]
    fn forgetting_to_return_a_table_gives_an_actionable_error() {
        // A very common mistake; the message should name the fix.
        let err = expect_load_error(plugin("local M = {}\n"));
        let text = err.to_string();
        assert!(text.contains("return M"), "unhelpful message: {text}");
    }

    #[test]
    fn a_syntax_error_is_reported_at_load_time() {
        let err = expect_load_error(plugin("this is not lua ===\n"));
        assert!(matches!(err, PluginError::Init { .. }));
    }

    #[test]
    fn startup_can_register_commands_and_keybinds() {
        let mut p = plugin(
            r#"
local M = {}
function M.on_startup(ctx)
  ctx.register_command("toggle", "Toggle it")
  ctx.register_keybind("ctrl+shift+b", "toggle")
end
return M
"#,
        )
        .unwrap();

        let commands = p.dispatch(&Event::Startup).unwrap();
        assert_eq!(
            commands,
            vec![
                Command::RegisterCommand {
                    name: "toggle".to_owned(),
                    description: "Toggle it".to_owned()
                },
                Command::RegisterKeybind {
                    chord: "ctrl+shift+b".to_owned(),
                    command: "toggle".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_missing_handler_is_not_an_error() {
        // Plugins implement only the events they care about.
        let mut p = plugin("return {}\n").unwrap();
        assert!(p.dispatch(&Event::Startup).unwrap().is_empty());
        assert!(p.dispatch(&Event::ConfigReload).unwrap().is_empty());
    }

    #[test]
    fn commands_are_queued_not_executed() {
        let mut p = plugin(
            r#"
return {
  on_config_reload = function(ctx)
    ctx.new_tab()
    ctx.split("right")
    ctx.send_text("ls\n")
  end
}
"#,
        )
        .unwrap();

        let commands = p.dispatch(&Event::ConfigReload).unwrap();
        assert_eq!(
            commands,
            vec![
                Command::NewTab,
                Command::Split {
                    direction: Direction::Right
                },
                Command::SendText {
                    pane: None,
                    text: "ls\n".to_owned()
                },
            ]
        );
    }

    #[test]
    fn commands_do_not_leak_between_dispatches() {
        let mut p =
            plugin("return { on_config_reload = function(ctx) ctx.new_tab() end }\n").unwrap();
        assert_eq!(p.dispatch(&Event::ConfigReload).unwrap().len(), 1);
        assert_eq!(
            p.dispatch(&Event::ConfigReload).unwrap().len(),
            1,
            "the queue must be drained each time, not accumulate"
        );
    }

    #[test]
    fn an_invalid_direction_is_reported_rather_than_guessed() {
        let mut p =
            plugin("return { on_config_reload = function(ctx) ctx.split(\"sideways\") end }\n")
                .unwrap();
        let err = p.dispatch(&Event::ConfigReload).unwrap_err();
        assert!(err.to_string().contains("sideways"), "{err}");
    }

    #[test]
    fn on_key_claims_the_key_only_on_an_explicit_true() {
        let mut p = plugin(
            r#"
return {
  on_key = function(ctx, key)
    if key.chord == "ctrl+shift+q" then return true end
  end
}
"#,
        )
        .unwrap();

        let claimed = Event::Key(KeyPress {
            chord: "ctrl+shift+q".to_owned(),
            modifiers: Default::default(),
        });
        let (outcome, _) = p.on_key(&claimed).unwrap();
        assert_eq!(outcome, KeyOutcome::Handled);

        // A handler returning nothing must not swallow the key.
        let other = Event::Key(KeyPress {
            chord: "a".to_owned(),
            modifiers: Default::default(),
        });
        let (outcome, _) = p.on_key(&other).unwrap();
        assert_eq!(outcome, KeyOutcome::Unhandled);
    }

    #[test]
    fn on_key_receives_the_modifier_state() {
        let mut p = plugin(
            r#"
return {
  on_key = function(ctx, key)
    if key.ctrl and key.shift and not key.alt then return true end
  end
}
"#,
        )
        .unwrap();

        let (outcome, _) = p
            .on_key(&Event::Key(KeyPress {
                chord: "ctrl+shift+x".to_owned(),
                modifiers: tuz_plugin_api::Modifiers {
                    ctrl: true,
                    shift: true,
                    alt: false,
                    super_key: false,
                },
            }))
            .unwrap();
        assert_eq!(outcome, KeyOutcome::Handled);
    }

    #[test]
    fn event_payloads_reach_the_handler() {
        let mut p = plugin(
            r#"
return {
  on_title_change = function(ctx, e)
    ctx.send_text(e.title .. ":" .. tostring(e.pane))
  end
}
"#,
        )
        .unwrap();

        let commands = p
            .dispatch(&Event::TitleChange {
                pane: PaneId(7),
                title: "vim".to_owned(),
            })
            .unwrap();
        assert_eq!(
            commands,
            vec![Command::SendText {
                pane: None,
                text: "vim:7".to_owned()
            }]
        );
    }

    #[test]
    fn status_segments_are_read_from_a_table_list() {
        let mut p = plugin(
            r##"
return {
  on_status_bar_render = function(ctx)
    ctx.set_status({
      { text = "left", foreground = "#ff0000" },
      { text = "right" },
    })
  end
}
"##,
        )
        .unwrap();

        let commands = p.dispatch(&Event::StatusBarRender).unwrap();
        assert_eq!(
            commands,
            vec![Command::SetStatusSegments {
                segments: vec![
                    StatusSegment {
                        id: None,
                        text: "left".to_owned(),
                        foreground: Some("#ff0000".to_owned()),
                        background: None,
                    },
                    StatusSegment {
                        id: None,
                        text: "right".to_owned(),
                        foreground: None,
                        background: None,
                    },
                ]
            }]
        );
    }

    #[test]
    fn a_runtime_error_in_a_handler_is_reported_not_fatal() {
        let mut p = plugin("return { on_startup = function(ctx) error(\"nope\") end }\n").unwrap();
        let err = p.dispatch(&Event::Startup).unwrap_err();
        assert!(matches!(err, PluginError::Runtime { .. }));
        assert!(err.to_string().contains("nope"));

        // And the plugin is still usable afterwards.
        assert!(p.dispatch(&Event::ConfigReload).is_ok());
    }

    #[test]
    fn an_infinite_loop_is_aborted_by_the_timeout() {
        // The property that keeps a bad plugin from freezing the terminal.
        let mut p =
            plugin("return { on_startup = function(ctx) while true do end end }\n").unwrap();

        let started = std::time::Instant::now();
        let err = p.dispatch(&Event::Startup).unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            matches!(err, PluginError::Timeout { .. }),
            "expected a timeout, got {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "took {elapsed:?}, the hook is not firing"
        );
    }

    #[test]
    fn each_call_gets_a_fresh_time_budget() {
        // Otherwise a long-lived plugin eventually times out on trivial work.
        let mut p = plugin(
            r#"
return {
  on_config_reload = function(ctx)
    local x = 0
    for i = 1, 200000 do x = x + i end
    ctx.new_tab()
  end
}
"#,
        )
        .unwrap();

        for round in 0..3 {
            assert!(
                p.dispatch(&Event::ConfigReload).is_ok(),
                "round {round} should not time out"
            );
        }
    }

    // --- sandbox ----------------------------------------------------------

    #[test]
    fn dangerous_loaders_are_always_removed() {
        // These bypass every other restriction, so no permission re-enables them.
        for global in ["dofile", "loadfile", "load", "require", "debug"] {
            let src = format!(
                "return {{ on_startup = function(ctx) if {global} ~= nil then error(\"{global} present\") end end }}"
            );
            let mut p = plugin_with(
                &src,
                vec![
                    Permission::SpawnProcess,
                    Permission::FsRead("/".to_owned()),
                    Permission::FsWrite("/".to_owned()),
                ],
            )
            .unwrap();
            assert!(
                p.dispatch(&Event::Startup).is_ok(),
                "`{global}` should have been removed even with all permissions"
            );
        }
    }

    #[test]
    fn os_exit_is_never_available() {
        // A plugin exiting the process would look like a terminal crash.
        let mut p = plugin_with(
            "return { on_startup = function(ctx) if os.exit ~= nil then error(\"os.exit present\") end end }",
            vec![Permission::SpawnProcess],
        )
        .unwrap();
        assert!(p.dispatch(&Event::Startup).is_ok());
    }

    #[test]
    fn io_is_withheld_without_a_filesystem_permission() {
        let mut p = plugin(
            "return { on_startup = function(ctx) if io ~= nil then error(\"io present\") end end }",
        )
        .unwrap();
        assert!(p.dispatch(&Event::Startup).is_ok());
    }

    #[test]
    fn io_is_available_once_a_filesystem_permission_is_granted() {
        let mut p = plugin_with(
            "return { on_startup = function(ctx) if io == nil then error(\"io missing\") end end }",
            vec![Permission::FsRead("/tmp".to_owned())],
        )
        .unwrap();
        assert!(
            p.dispatch(&Event::Startup).is_ok(),
            "a plugin granted fs-read should be able to read files"
        );
    }

    #[test]
    fn os_execute_is_withheld_without_spawn_process() {
        let mut p = plugin(
            "return { on_startup = function(ctx) if os.execute ~= nil then error(\"execute present\") end end }",
        )
        .unwrap();
        assert!(p.dispatch(&Event::Startup).is_ok());
    }

    #[test]
    fn os_execute_is_available_with_spawn_process() {
        let mut p = plugin_with(
            "return { on_startup = function(ctx) if os.execute == nil then error(\"execute missing\") end end }",
            vec![Permission::SpawnProcess],
        )
        .unwrap();
        assert!(p.dispatch(&Event::Startup).is_ok());
    }

    #[test]
    fn safe_standard_library_functions_remain_usable() {
        // The sandbox must not be so aggressive that ordinary Lua stops working.
        let mut p = plugin(
            r#"
return {
  on_startup = function(ctx)
    local s = string.format("%d", 42)
    local t = { 3, 1, 2 }
    table.sort(t)
    ctx.send_text(s .. tostring(t[1]) .. tostring(math.floor(1.5)))
  end
}
"#,
        )
        .unwrap();

        let commands = p.dispatch(&Event::Startup).unwrap();
        assert_eq!(
            commands,
            vec![Command::SendText {
                pane: None,
                text: "4211".to_owned()
            }]
        );
    }

    #[test]
    fn a_key_event_sent_to_dispatch_produces_nothing() {
        // Keys go through `on_key`; routing them here too would double-deliver.
        let mut p = plugin("return { on_key = function() return true end }\n").unwrap();
        let commands = p
            .dispatch(&Event::Key(KeyPress {
                chord: "a".to_owned(),
                modifiers: Default::default(),
            }))
            .unwrap();
        assert!(commands.is_empty());
    }
    /// Every `Command` variant must be reachable from Lua.
    ///
    /// Five were not: a Lua plugin could not target a specific pane, resize a split,
    /// or set a config overlay, while a WASM plugin could — the two runtimes serve
    /// one API and are supposed to have the same reach.
    #[test]
    fn lua_can_emit_every_command_wasm_can() {
        let source = r#"
            local M = {}
            function M.on_startup(ctx)
              ctx.focus_pane(3)
              ctx.close_pane_id(4)
              ctx.send_text_to(5, "hi")
              ctx.resize("right", 0.25)
              ctx.set_config("[font]\nsize = 20.0\n")
            end
            return M
        "#;
        let mut plugin = plugin(source).expect("should load");
        let commands = plugin.dispatch(&Event::Startup).expect("should run");

        assert!(commands.contains(&Command::FocusPane { pane: PaneId(3) }));
        assert!(commands.contains(&Command::ClosePane {
            pane: Some(PaneId(4))
        }));
        assert!(commands.contains(&Command::SendText {
            pane: Some(PaneId(5)),
            text: "hi".to_owned()
        }));
        assert!(commands.iter().any(|c| matches!(
            c,
            Command::Resize {
                direction: Direction::Right,
                ..
            }
        )));
        assert!(commands
            .iter()
            .any(|c| matches!(c, Command::SetConfigOverlay { .. })));
    }

    #[test]
    fn a_bad_pane_id_is_an_error_rather_than_a_wrapped_number() {
        // `-1 as u32` is four billion, which would target a pane that does not exist
        // and fail silently in the app.
        let source = r#"
            local M = {}
            function M.on_startup(ctx) ctx.focus_pane(-1) end
            return M
        "#;
        let mut plugin = plugin(source).expect("should load");
        assert!(plugin.dispatch(&Event::Startup).is_err());
    }
}
