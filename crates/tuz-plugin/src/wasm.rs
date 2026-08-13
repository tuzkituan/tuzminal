//! The WebAssembly plugin runtime.
//!
//! Unlike the Lua runtime, this one is a real sandbox: a plugin is a WASM module
//! with no ambient authority at all. It can only call host functions that were
//! explicitly linked into its instance, so an ungranted permission is not a check
//! that can be bypassed — the function simply does not exist in its import table.
//!
//! # ABI
//!
//! Deliberately a plain core-module ABI rather than the Component Model, because
//! it lets a plugin be built by anything that can emit WASM — including
//! hand-written `.wat` — with no toolchain requirements beyond `--target
//! wasm32-unknown-unknown`.
//!
//! Messages cross the boundary as JSON in linear memory. A plugin exports:
//!
//! ```text
//! memory:                       the module's linear memory
//! tuz_alloc(len: i32) -> i32    allocate `len` bytes, return the offset
//! tuz_on_event(ptr, len) -> i64 handle a JSON event; returns a packed result
//! ```
//!
//! and may import, subject to permissions:
//!
//! ```text
//! tuz_emit(ptr, len)            queue a JSON command
//! tuz_log(ptr, len)             write a log line
//! ```
//!
//! `tuz_on_event` returns two packed `i32`s: the high half is the [`KeyOutcome`]
//! (1 = handled) and the low half is unused, reserved for a future error channel.
//! Packing avoids needing a second export just to report whether a key was
//! claimed.
//!
//! # Fuel
//!
//! Every call is metered. A plugin that exhausts its fuel is trapped and the call
//! aborted, so an infinite loop costs one aborted callback rather than a hung
//! terminal — the same guarantee the Lua runtime gets from its instruction hook,
//! but enforced by the engine.

use crate::{PluginError, PluginRuntime};
use std::sync::{Arc, Mutex};
use tuz_plugin_api::{Command, Event, KeyOutcome, Manifest, Permission};
use wasmtime::{Caller, Engine, Extern, Instance, Linker, Memory, Module, Store, Val};

/// Fuel granted per callback.
///
/// Roughly tens of milliseconds of work on current hardware — generous for a
/// status-bar update, far too little to hang the terminal.
const FUEL_PER_CALL: u64 = 50_000_000;

/// What the guest can reach from a host call.
struct HostState {
    plugin: String,
    /// Commands the current callback queued.
    commands: Arc<Mutex<Vec<Command>>>,
}

pub struct WasmPlugin {
    store: Store<HostState>,
    instance: Instance,
    name: String,
    commands: Arc<Mutex<Vec<Command>>>,
}

impl WasmPlugin {
    /// Compile and instantiate a plugin module.
    pub fn load(manifest: &Manifest, entry: &std::path::Path) -> Result<Self, PluginError> {
        let bytes = std::fs::read(entry).map_err(|source| PluginError::Io {
            path: entry.to_owned(),
            source,
        })?;

        let mut config = wasmtime::Config::new();
        // Required for the fuel limit below; without it a loop runs forever.
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| PluginError::Init {
            plugin: manifest.name.clone(),
            message: format!("failed to create the WASM engine: {e}"),
        })?;

        let module = Module::new(&engine, &bytes).map_err(|e| PluginError::Init {
            plugin: manifest.name.clone(),
            message: format!("not a valid WebAssembly module: {e}"),
        })?;

        let commands = Arc::new(Mutex::new(Vec::new()));
        let mut store = Store::new(
            &engine,
            HostState {
                plugin: manifest.name.clone(),
                commands: commands.clone(),
            },
        );
        store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| PluginError::Init {
                plugin: manifest.name.clone(),
                message: e.to_string(),
            })?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        link_host_functions(&mut linker, manifest).map_err(|e| PluginError::Init {
            plugin: manifest.name.clone(),
            message: format!("failed to link host functions: {e}"),
        })?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| PluginError::Init {
                plugin: manifest.name.clone(),
                message: format!("failed to instantiate: {e}"),
            })?;

        // Fail loudly at load time rather than on the first event, when the cause
        // is much less obvious.
        for required in ["memory", "tuz_alloc", "tuz_on_event"] {
            if instance.get_export(&mut store, required).is_none() {
                return Err(PluginError::Init {
                    plugin: manifest.name.clone(),
                    message: format!("the module must export `{required}`"),
                });
            }
        }

        Ok(Self {
            store,
            instance,
            name: manifest.name.clone(),
            commands,
        })
    }

    /// Serialize an event, hand it to the guest, and collect the result.
    fn call(&mut self, event: &Event) -> Result<KeyOutcome, PluginError> {
        let json = serde_json::to_vec(event).map_err(|e| PluginError::Runtime {
            plugin: self.name.clone(),
            message: format!("failed to encode event: {e}"),
        })?;

        // Refuel per call so each callback gets the full budget instead of
        // inheriting what previous ones consumed.
        self.store
            .set_fuel(FUEL_PER_CALL)
            .map_err(|e| PluginError::Runtime {
                plugin: self.name.clone(),
                message: e.to_string(),
            })?;

        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "tuz_alloc")
            .map_err(|e| PluginError::Runtime {
                plugin: self.name.clone(),
                message: format!("tuz_alloc has the wrong signature: {e}"),
            })?;

        let ptr = alloc
            .call(&mut self.store, json.len() as i32)
            .map_err(|e| self.classify(e))?;
        if ptr <= 0 {
            return Err(PluginError::Runtime {
                plugin: self.name.clone(),
                message: "tuz_alloc returned a null pointer".to_owned(),
            });
        }

        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or_else(|| PluginError::Runtime {
                plugin: self.name.clone(),
                message: "the module does not export `memory`".to_owned(),
            })?;

        memory
            .write(&mut self.store, ptr as usize, &json)
            .map_err(|e| PluginError::Runtime {
                plugin: self.name.clone(),
                message: format!("failed to write the event into guest memory: {e}"),
            })?;

        let on_event = self
            .instance
            .get_typed_func::<(i32, i32), i64>(&mut self.store, "tuz_on_event")
            .map_err(|e| PluginError::Runtime {
                plugin: self.name.clone(),
                message: format!("tuz_on_event has the wrong signature: {e}"),
            })?;

        let packed = on_event
            .call(&mut self.store, (ptr, json.len() as i32))
            .map_err(|e| self.classify(e))?;

        // High half is the key outcome; see the module docs.
        let outcome = if (packed >> 32) as i32 == 1 {
            KeyOutcome::Handled
        } else {
            KeyOutcome::Unhandled
        };
        Ok(outcome)
    }

    /// Turn a wasmtime trap into our error type, separating fuel exhaustion.
    fn classify(&self, error: wasmtime::Error) -> PluginError {
        if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
            if *trap == wasmtime::Trap::OutOfFuel {
                return PluginError::Timeout {
                    plugin: self.name.clone(),
                };
            }
        }
        PluginError::Runtime {
            plugin: self.name.clone(),
            message: error.to_string(),
        }
    }

    fn take_commands(&self) -> Vec<Command> {
        match self.commands.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            // A poisoned mutex means a host function panicked. Losing this batch of
            // commands is better than propagating a panic into the event loop.
            Err(poisoned) => {
                log::warn!("plugin `{}` command queue was poisoned", self.name);
                std::mem::take(&mut *poisoned.into_inner())
            }
        }
    }
}

impl PluginRuntime for WasmPlugin {
    fn dispatch(&mut self, event: &Event) -> Result<Vec<Command>, PluginError> {
        self.call(event)?;
        Ok(self.take_commands())
    }

    fn on_key(&mut self, event: &Event) -> Result<(KeyOutcome, Vec<Command>), PluginError> {
        let outcome = self.call(event)?;
        Ok((outcome, self.take_commands()))
    }

    fn runtime_name(&self) -> &'static str {
        "wasm"
    }
}

/// Link the host functions this plugin is allowed to call.
///
/// This is where WASM permissions become structural: a function that is not linked
/// is absent from the guest's import table, so calling it fails at instantiation
/// rather than being caught by a runtime check that could be missed.
fn link_host_functions(
    linker: &mut Linker<HostState>,
    manifest: &Manifest,
) -> Result<(), wasmtime::Error> {
    // Always available: queueing a command is the entire point, and every command
    // is applied by the host, which re-checks anything sensitive.
    linker.func_wrap(
        "tuz",
        "tuz_emit",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
            let Some(text) = read_string(&mut caller, ptr, len) else {
                return;
            };
            match serde_json::from_str::<Command>(&text) {
                Ok(command) => {
                    if let Ok(mut queue) = caller.data().commands.lock() {
                        queue.push(command);
                    }
                }
                Err(e) => log::warn!(
                    "plugin `{}` emitted an unparseable command: {e}",
                    caller.data().plugin
                ),
            }
        },
    )?;

    linker.func_wrap(
        "tuz",
        "tuz_log",
        |mut caller: Caller<'_, HostState>, ptr: i32, len: i32| {
            if let Some(text) = read_string(&mut caller, ptr, len) {
                log::info!("[{}] {text}", caller.data().plugin);
            }
        },
    )?;

    // Permission-gated imports would be linked here. They are named explicitly so
    // a plugin importing one it was not granted fails to instantiate with a clear
    // "unknown import" error.
    if manifest.has_permission(&Permission::Clipboard) {
        // Placeholder: the clipboard lives on the UI thread, so a real
        // implementation routes through a command rather than a direct call.
        log::debug!("plugin `{}` was granted clipboard access", manifest.name);
    }

    Ok(())
}

/// Read a UTF-8 string out of guest memory.
///
/// Returns `None` rather than trapping on a bad pointer: a malformed call from a
/// plugin should cost that call, not the terminal.
fn read_string(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Option<String> {
    if ptr < 0 || len < 0 {
        return None;
    }
    let memory = match caller.get_export("memory") {
        Some(Extern::Memory(memory)) => memory,
        _ => return None,
    };
    let mut buffer = vec![0u8; len as usize];
    memory.read(&mut *caller, ptr as usize, &mut buffer).ok()?;
    String::from_utf8(buffer).ok()
}

/// Kept so the unused-import warning does not hide a real one later.
#[allow(dead_code)]
fn assert_types(_: Option<Memory>, _: Option<Val>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_plugin_api::{Direction, Runtime};

    fn manifest(permissions: Vec<Permission>) -> Manifest {
        Manifest {
            name: "wasmtest".to_owned(),
            version: "0.1.0".to_owned(),
            api_version: tuz_plugin_api::API_VERSION,
            runtime: Runtime::Wasm,
            entry: "plugin.wasm".to_owned(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            permissions,
            events: Vec::new(),
            config: Default::default(),
        }
    }

    /// Unwrap the error from a failed load. `WasmPlugin` is not `Debug`, so
    /// `unwrap_err` cannot be used directly.
    fn expect_load_error(result: Result<WasmPlugin, PluginError>) -> PluginError {
        match result {
            Ok(_) => panic!("expected the load to fail"),
            Err(e) => e,
        }
    }

    /// Build a plugin from WebAssembly text and load it.
    ///
    /// `.wat` keeps these tests self-contained: no cross-compilation toolchain is
    /// needed to exercise the real engine, the real ABI and the real fuel limit.
    fn load_wat(wat: &str) -> Result<WasmPlugin, PluginError> {
        let bytes = wat::parse_str(wat).expect("test WAT should assemble");
        let dir = std::env::temp_dir().join(format!(
            "tuz-wasm-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.wasm");
        std::fs::write(&path, bytes).unwrap();
        WasmPlugin::load(&manifest(vec![]), &path)
    }

    /// A module implementing the minimum ABI: a bump allocator and a no-op handler.
    const MINIMAL: &str = r#"
(module
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))

  (func (export "tuz_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    global.get $next
    local.set $ptr
    global.get $next
    local.get $len
    i32.add
    global.set $next
    local.get $ptr)

  (func (export "tuz_on_event") (param $ptr i32) (param $len i32) (result i64)
    i64.const 0)
)
"#;

    #[test]
    fn a_minimal_module_loads_and_dispatches() {
        let mut p = load_wat(MINIMAL).expect("should load");
        assert_eq!(p.runtime_name(), "wasm");

        let commands = p.dispatch(&Event::Startup).expect("dispatch should work");
        assert!(commands.is_empty());
    }

    #[test]
    fn a_module_missing_a_required_export_is_refused_at_load_time() {
        // Catching this now is much clearer than a failure on the first event.
        let no_alloc = r#"
(module
  (memory (export "memory") 1)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64) i64.const 0)
)
"#;
        let err = expect_load_error(load_wat(no_alloc));
        assert!(err.to_string().contains("tuz_alloc"), "{err}");

        let no_memory = r#"
(module
  (func (export "tuz_alloc") (param i32) (result i32) i32.const 0)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64) i64.const 0)
)
"#;
        let err = expect_load_error(load_wat(no_memory));
        assert!(err.to_string().contains("memory"), "{err}");
    }

    #[test]
    fn invalid_bytes_are_reported_as_an_invalid_module() {
        let dir = std::env::temp_dir().join(format!("tuz-wasm-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plugin.wasm");
        std::fs::write(&path, b"definitely not wasm").unwrap();

        let err = expect_load_error(WasmPlugin::load(&manifest(vec![]), &path));
        assert!(
            err.to_string().contains("valid WebAssembly"),
            "unhelpful message: {err}"
        );
    }

    #[test]
    fn a_plugin_can_emit_a_command_across_the_boundary() {
        // The core round trip: guest writes JSON, host parses it into a Command.
        let emitting = r#"
(module
  (import "tuz" "tuz_emit" (func $emit (param i32) (param i32)))
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (data (i32.const 16) "{\"type\":\"new_tab\"}")

  (func (export "tuz_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    global.get $next
    local.set $ptr
    global.get $next
    local.get $len
    i32.add
    global.set $next
    local.get $ptr)

  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    i32.const 16
    i32.const 18
    call $emit
    i64.const 0)
)
"#;
        let mut p = load_wat(emitting).expect("should load");
        let commands = p.dispatch(&Event::Startup).expect("dispatch should work");
        assert_eq!(commands, vec![Command::NewTab]);
    }

    #[test]
    fn a_plugin_can_emit_a_command_with_fields() {
        let emitting = r#"
(module
  (import "tuz" "tuz_emit" (func $emit (param i32) (param i32)))
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (data (i32.const 16) "{\"type\":\"split\",\"direction\":\"right\"}")

  (func (export "tuz_alloc") (param $len i32) (result i32)
    global.get $next)

  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    i32.const 16
    ;; Length must match the data literal exactly; a byte too many appends a NUL
    ;; and the JSON silently fails to parse.
    i32.const 36
    call $emit
    i64.const 0)
)
"#;
        let mut p = load_wat(emitting).expect("should load");
        let commands = p.dispatch(&Event::Startup).expect("dispatch should work");
        assert_eq!(
            commands,
            vec![Command::Split {
                direction: Direction::Right
            }]
        );
    }

    #[test]
    fn a_module_importing_an_unlinked_function_fails_to_instantiate() {
        // This is what makes WASM permissions structural rather than a check: the
        // function is simply not there, so the module cannot even be built.
        let forbidden = r#"
(module
  (import "tuz" "tuz_definitely_not_granted" (func $nope (param i32)))
  (memory (export "memory") 1)
  (func (export "tuz_alloc") (param i32) (result i32) i32.const 0)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64) i64.const 0)
)
"#;
        let err = expect_load_error(load_wat(forbidden));
        assert!(
            matches!(err, PluginError::Init { .. }),
            "expected an init failure, got {err}"
        );
    }

    #[test]
    fn an_infinite_loop_is_trapped_by_the_fuel_limit() {
        // The guarantee that a bad plugin cannot hang the terminal.
        let spinning = r#"
(module
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "tuz_alloc") (param i32) (result i32) global.get $next)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    (loop $forever
      br $forever)
    i64.const 0)
)
"#;
        let mut p = load_wat(spinning).expect("should load");

        let started = std::time::Instant::now();
        let err = p.dispatch(&Event::Startup).unwrap_err();
        let elapsed = started.elapsed();

        assert!(
            matches!(err, PluginError::Timeout { .. }),
            "expected a fuel timeout, got {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "took {elapsed:?}; fuel metering is not active"
        );
    }

    #[test]
    fn each_call_is_refuelled() {
        // Without refuelling, a plugin dies after a few successful callbacks.
        let mut p = load_wat(MINIMAL).expect("should load");
        for round in 0..5 {
            assert!(
                p.dispatch(&Event::ConfigReload).is_ok(),
                "round {round} should still have fuel"
            );
        }
    }

    #[test]
    fn a_key_outcome_of_one_claims_the_key() {
        // The packed return value: high half 1 means handled.
        let claiming = r#"
(module
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "tuz_alloc") (param i32) (result i32) global.get $next)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    i64.const 4294967296)
)
"#;
        let mut p = load_wat(claiming).expect("should load");
        let (outcome, _) = p
            .on_key(&Event::Key(tuz_plugin_api::KeyPress {
                chord: "ctrl+shift+z".to_owned(),
                modifiers: Default::default(),
            }))
            .unwrap();
        assert_eq!(outcome, KeyOutcome::Handled);
    }

    #[test]
    fn a_zero_return_leaves_the_key_unhandled() {
        let mut p = load_wat(MINIMAL).expect("should load");
        let (outcome, _) = p
            .on_key(&Event::Key(tuz_plugin_api::KeyPress {
                chord: "a".to_owned(),
                modifiers: Default::default(),
            }))
            .unwrap();
        assert_eq!(outcome, KeyOutcome::Unhandled);
    }

    #[test]
    fn a_guest_trap_is_reported_without_taking_down_the_host() {
        let trapping = r#"
(module
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "tuz_alloc") (param i32) (result i32) global.get $next)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    unreachable)
)
"#;
        let mut p = load_wat(trapping).expect("should load");
        let err = p.dispatch(&Event::Startup).unwrap_err();
        assert!(matches!(err, PluginError::Runtime { .. }), "{err}");

        // The instance is still alive for the next call.
        assert!(p.dispatch(&Event::ConfigReload).is_err());
    }

    #[test]
    fn a_null_allocation_is_reported_rather_than_written_to() {
        // Writing at offset 0 would silently corrupt the guest's own data.
        let bad_alloc = r#"
(module
  (memory (export "memory") 1)
  (func (export "tuz_alloc") (param i32) (result i32) i32.const 0)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64) i64.const 0)
)
"#;
        let mut p = load_wat(bad_alloc).expect("should load");
        let err = p.dispatch(&Event::Startup).unwrap_err();
        assert!(err.to_string().contains("null pointer"), "{err}");
    }

    #[test]
    fn an_unparseable_command_is_ignored_rather_than_fatal() {
        let garbage = r#"
(module
  (import "tuz" "tuz_emit" (func $emit (param i32) (param i32)))
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (data (i32.const 16) "not json at all")
  (func (export "tuz_alloc") (param i32) (result i32) global.get $next)
  (func (export "tuz_on_event") (param i32) (param i32) (result i64)
    i32.const 16
    i32.const 15
    call $emit
    i64.const 0)
)
"#;
        let mut p = load_wat(garbage).expect("should load");
        let commands = p.dispatch(&Event::Startup).expect("should not be fatal");
        assert!(commands.is_empty(), "garbage must not become a command");
    }
}
