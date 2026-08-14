//! The plugin host: discovery, dispatch and the runtime abstraction.
//!
//! ```text
//!   plugin dirs ──► discover() ──► Manifest ──► Runtime::load ──► LoadedPlugin
//!                                                                     │
//!   Event ──► Host::dispatch ──┬──► Lua state  ──┐                     │
//!                              └──► WASM instance┴──► Vec<Command> ────┘
//!                                                        │
//!                                              drained by the main thread
//! ```
//!
//! # The two runtimes are not equivalent, and the docs say so
//!
//! [`Runtime::Wasm`] permissions are structural: an ungranted host function is
//! never linked into the instance, so it cannot be called. [`Runtime::Lua`]
//! permissions are a restricted global environment, which stops accidents but is
//! **not a security boundary** — installing a Lua plugin means trusting its code.
//! [`Host::load_all`] logs that distinction at load time rather than leaving users
//! to assume a guarantee that does not exist.
//!
//! # Timeouts
//!
//! Every callback runs under a budget. Lua uses an instruction-count hook; WASM
//! uses fuel. A plugin that exceeds its budget has the call aborted and is
//! disabled after repeated offences, so a bad plugin cannot wedge the terminal.

#[cfg(feature = "lua")]
pub mod lua;
#[cfg(feature = "wasm")]
pub mod wasm;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tuz_plugin_api::{Command, Event, KeyOutcome, Manifest, ManifestError, Runtime};

/// How a plugin's code is executed.
///
/// Implemented once per runtime; the host only ever sees this trait, which is what
/// keeps [`Event`] and [`Command`] the single contract.
///
/// Deliberately **not** `Send`. The host runs on the UI thread, which the original
/// design did not assume — but moving plugins to their own thread buys nothing
/// here and costs a lot: `on_key` needs a synchronous answer, so an off-thread host
/// would need a request/response handshake with a deadline on every keystroke, and
/// the deadline is the only thing actually protecting the frame. Running inline
/// with a hard per-callback budget gives the same protection with none of the
/// cross-thread machinery, and it lets the Lua runtime use mlua's cheaper
/// single-threaded build.
pub trait PluginRuntime {
    /// Deliver an event and collect whatever commands it emitted.
    fn dispatch(&mut self, event: &Event) -> Result<Vec<Command>, PluginError>;

    /// Deliver a key press and report whether the plugin claimed it.
    ///
    /// Separate from [`dispatch`](PluginRuntime::dispatch) because it needs an
    /// answer, and every keystroke waits for it.
    fn on_key(&mut self, event: &Event) -> Result<(KeyOutcome, Vec<Command>), PluginError>;

    /// Human-readable runtime name, for logs.
    fn runtime_name(&self) -> &'static str;
}

/// A plugin that loaded successfully.
pub struct LoadedPlugin {
    pub manifest: Manifest,
    pub directory: PathBuf,
    runtime: Box<dyn PluginRuntime>,
    /// Commands the plugin registered, so the keymap can resolve their names.
    registered_commands: Vec<String>,
    /// Status segments from the plugin's last `StatusBarRender`.
    status: Vec<tuz_plugin_api::StatusSegment>,
    /// Consecutive callback failures. A plugin that keeps failing is disabled
    /// rather than logging on every keystroke forever.
    failures: u32,
    disabled: bool,
}

impl LoadedPlugin {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
    pub fn is_disabled(&self) -> bool {
        self.disabled
    }
    pub fn registered_commands(&self) -> &[String] {
        &self.registered_commands
    }
    pub fn status_segments(&self) -> &[tuz_plugin_api::StatusSegment] {
        &self.status
    }
}

/// How many consecutive failures disable a plugin.
const FAILURE_LIMIT: u32 = 3;

/// Owns every loaded plugin and routes events to them.
pub struct Host {
    plugins: Vec<LoadedPlugin>,
    /// Chord -> fully qualified command name (`plugin.command`).
    keybinds: HashMap<String, String>,
    /// Budget for a normal callback.
    callback_timeout: Duration,
    /// Budget for the synchronous key hook, kept small because typing waits on it.
    key_timeout: Duration,
}

impl Host {
    pub fn new(callback_timeout: Duration, key_timeout: Duration) -> Self {
        Self {
            plugins: Vec::new(),
            keybinds: HashMap::new(),
            callback_timeout,
            key_timeout,
        }
    }

    /// A host with no plugins, for when plugins are disabled in config.
    pub fn disabled() -> Self {
        Self::new(Duration::from_millis(250), Duration::from_millis(5))
    }

    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Whether any enabled plugin asked for an event by name.
    ///
    /// Lets the app skip *producing* an expensive event rather than producing it and
    /// having [`Host::dispatch`] drop it. Building an `input_line` event means taking
    /// the terminal mutex and copying a row, which is not worth doing for nobody.
    ///
    /// Disabled plugins do not count: one that failed its way out of the session must
    /// not keep the cost of its events alive.
    pub fn wants(&self, event: &str) -> bool {
        self.plugins
            .iter()
            .any(|p| !p.disabled && p.manifest.wants_event(event))
    }

    /// Every command name any plugin registered, for keymap resolution.
    pub fn command_names(&self) -> Vec<String> {
        self.plugins
            .iter()
            .flat_map(|p| p.registered_commands.iter().cloned())
            .collect()
    }

    /// Deliver a segment click to the plugin that published it.
    ///
    /// Targeted rather than broadcast: a click belongs to one segment, and sending it
    /// to every plugin would make two plugins with a segment called `open` both act
    /// on one press. The qualified form is `plugin.id`, matching how commands and
    /// keybinds are namespaced.
    pub fn click_status_segment(&mut self, qualified: &str) -> Vec<tuz_plugin_api::Command> {
        let Some((plugin_name, id)) = qualified.split_once('.') else {
            return Vec::new();
        };
        let Some(index) = self
            .plugins
            .iter()
            .position(|p| !p.disabled && p.manifest.name == plugin_name)
        else {
            return Vec::new();
        };

        let event = Event::StatusSegmentClick { id: id.to_owned() };
        let plugin = &mut self.plugins[index];
        match plugin.runtime.dispatch(&event) {
            Ok(commands) => {
                for command in &commands {
                    if let Command::SetStatusSegments { segments } = command {
                        plugin.status = segments.clone();
                    }
                }
                commands
            }
            Err(e) => {
                log::warn!(
                    "plugin `{}` failed handling a click: {e}",
                    plugin.manifest.name
                );
                Vec::new()
            }
        }
    }

    /// Status segments from all plugins, paired with the qualified id of any that
    /// can be clicked.
    ///
    /// The qualification happens here rather than in the plugin so a plugin cannot
    /// claim another's namespace by choosing a clever id.
    pub fn status_segments_with_owner(
        &self,
    ) -> Vec<(tuz_plugin_api::StatusSegment, Option<String>)> {
        self.plugins
            .iter()
            .filter(|p| !p.disabled)
            .flat_map(|p| {
                p.status.iter().map(move |segment| {
                    let owner = segment
                        .id
                        .as_ref()
                        .map(|id| format!("{}.{id}", p.manifest.name));
                    (segment.clone(), owner)
                })
            })
            .collect()
    }

    /// Status segments from all plugins, in load order.
    pub fn status_segments(&self) -> Vec<tuz_plugin_api::StatusSegment> {
        self.plugins
            .iter()
            .filter(|p| !p.disabled)
            .flat_map(|p| p.status.iter().cloned())
            .collect()
    }

    /// Find and load every enabled plugin under `dirs`.
    ///
    /// A plugin that fails to load is reported and skipped; one broken plugin must
    /// not cost the user the others.
    /// Drop every loaded plugin and load again from disk.
    ///
    /// Exists so enabling or disabling a plugin takes effect now rather than at the
    /// next launch. Dropping the runtimes discards their state — a plugin that was
    /// counting something starts over — which is the honest meaning of toggling it
    /// off and on, and is why this is not called on an ordinary config reload.
    ///
    /// The caller must rebuild the keymap afterwards: registered commands and
    /// keybinds are cleared here and re-registered by the loads.
    pub fn reload(&mut self, dirs: &[PathBuf], cfg: &tuz_config::Plugins) -> Vec<PluginError> {
        self.plugins.clear();
        self.keybinds.clear();
        self.load_all(dirs, cfg)
    }

    pub fn load_all(&mut self, dirs: &[PathBuf], cfg: &tuz_config::Plugins) -> Vec<PluginError> {
        let mut errors = Vec::new();

        for candidate in discover(dirs) {
            let (directory, manifest) = match candidate {
                Ok(found) => found,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            };

            // `load` acts as an allowlist when non-empty; `disable` always wins.
            if !cfg.load.is_empty() && !cfg.load.contains(&manifest.name) {
                log::debug!("skipping plugin `{}`: not in the load list", manifest.name);
                continue;
            }
            if cfg.disable.contains(&manifest.name) {
                log::debug!("skipping plugin `{}`: disabled in config", manifest.name);
                continue;
            }

            match self.load_one(&directory, manifest) {
                Ok(name) => log::info!("loaded plugin `{name}`"),
                Err(e) => {
                    log::warn!("plugin failed to load: {e}");
                    errors.push(e);
                }
            }
        }
        errors
    }

    fn load_one(&mut self, directory: &Path, manifest: Manifest) -> Result<String, PluginError> {
        let entry = directory.join(&manifest.entry);
        if !entry.is_file() {
            return Err(PluginError::MissingEntry {
                plugin: manifest.name.clone(),
                path: entry,
            });
        }

        // Say plainly what the permission grant does and does not guarantee, so a
        // user is not left assuming a sandbox that is not there.
        if !manifest.permissions.is_empty() {
            let described: Vec<String> =
                manifest.permissions.iter().map(|p| p.describe()).collect();
            match manifest.runtime {
                Runtime::Wasm => log::info!(
                    "plugin `{}` is sandboxed and granted: {}",
                    manifest.name,
                    described.join("; ")
                ),
                Runtime::Lua => log::warn!(
                    "plugin `{}` is Lua, which is NOT sandboxed; it can do anything \
                     your user account can, and it asked to: {}",
                    manifest.name,
                    described.join("; ")
                ),
            }
        }

        let runtime: Box<dyn PluginRuntime> = match manifest.runtime {
            #[cfg(feature = "lua")]
            Runtime::Lua => Box::new(lua::LuaPlugin::load(
                &manifest,
                &entry,
                self.callback_timeout,
            )?),
            #[cfg(not(feature = "lua"))]
            Runtime::Lua => {
                return Err(PluginError::RuntimeUnavailable {
                    plugin: manifest.name.clone(),
                    runtime: "lua",
                })
            }

            #[cfg(feature = "wasm")]
            Runtime::Wasm => Box::new(wasm::WasmPlugin::load(&manifest, &entry)?),
            #[cfg(not(feature = "wasm"))]
            Runtime::Wasm => {
                return Err(PluginError::RuntimeUnavailable {
                    plugin: manifest.name.clone(),
                    runtime: "wasm",
                })
            }
        };

        let name = manifest.name.clone();
        self.plugins.push(LoadedPlugin {
            manifest,
            directory: directory.to_owned(),
            runtime,
            registered_commands: Vec::new(),
            status: Vec::new(),
            failures: 0,
            disabled: false,
        });

        // Startup goes to *this* plugin only. Broadcasting it re-ran every already
        // loaded plugin's `on_startup` once per subsequent load, and — because
        // registrations were credited to the last plugin in the list — filed their
        // keybinds under whichever plugin happened to load last.
        let index = self.plugins.len() - 1;
        let commands = self.dispatch_to(index, &Event::Startup);
        self.apply_registrations_for(index, commands);
        Ok(name)
    }

    /// Deliver an event to one plugin, by index.
    ///
    /// The single-plugin half of `dispatch`, so a load can start one plugin without
    /// restarting the ones already running.
    fn dispatch_to(&mut self, index: usize, event: &Event) -> Vec<Command> {
        let Some(plugin) = self.plugins.get_mut(index) else {
            return Vec::new();
        };
        match plugin.runtime.dispatch(event) {
            Ok(commands) => {
                for command in &commands {
                    if let Command::SetStatusSegments { segments } = command {
                        plugin.status = segments.clone();
                    }
                }
                plugin.failures = 0;
                commands
            }
            Err(e) => {
                log::warn!("plugin `{}` failed: {e}", plugin.manifest.name);
                plugin.failures += 1;
                if plugin.failures >= FAILURE_LIMIT {
                    log::warn!("disabling plugin `{}`", plugin.manifest.name);
                    plugin.disabled = true;
                }
                Vec::new()
            }
        }
    }

    /// Deliver an event to every interested plugin and collect their commands.
    pub fn dispatch(&mut self, event: &Event) -> Vec<Command> {
        let name = event_name(event);
        let mut out = Vec::new();

        for plugin in &mut self.plugins {
            if plugin.disabled || !plugin.manifest.wants_event(name) {
                continue;
            }

            let started = Instant::now();
            match plugin.runtime.dispatch(event) {
                Ok(commands) => {
                    plugin.failures = 0;
                    // Capture status segments here rather than making the caller
                    // hunt for them among the returned commands.
                    for command in &commands {
                        if let Command::SetStatusSegments { segments } = command {
                            plugin.status = segments.clone();
                        }
                    }
                    out.extend(commands);
                }
                Err(e) => {
                    plugin.failures += 1;
                    log::warn!(
                        "plugin `{}` failed handling {name}: {e}",
                        plugin.manifest.name
                    );
                    if plugin.failures >= FAILURE_LIMIT {
                        log::error!(
                            "disabling plugin `{}` after {} consecutive failures",
                            plugin.manifest.name,
                            plugin.failures
                        );
                        plugin.disabled = true;
                    }
                }
            }

            let elapsed = started.elapsed();
            if elapsed > self.callback_timeout {
                log::warn!(
                    "plugin `{}` took {elapsed:?} handling {name}, over its {:?} budget",
                    plugin.manifest.name,
                    self.callback_timeout
                );
            }
        }
        out
    }

    /// Offer a key press to plugins, stopping at the first that claims it.
    ///
    /// Returns the outcome and any commands. Every keystroke waits on this, so it
    /// is bounded by `key_timeout` and stops at the first claim rather than
    /// polling every plugin.
    pub fn on_key(&mut self, event: &Event) -> (KeyOutcome, Vec<Command>) {
        let mut out = Vec::new();

        for plugin in &mut self.plugins {
            if plugin.disabled || !plugin.manifest.wants_event("key") {
                continue;
            }

            let started = Instant::now();
            match plugin.runtime.on_key(event) {
                Ok((outcome, commands)) => {
                    plugin.failures = 0;
                    out.extend(commands);
                    if outcome == KeyOutcome::Handled {
                        return (KeyOutcome::Handled, out);
                    }
                }
                Err(e) => {
                    plugin.failures += 1;
                    log::warn!("plugin `{}` failed on key: {e}", plugin.manifest.name);
                    if plugin.failures >= FAILURE_LIMIT {
                        plugin.disabled = true;
                    }
                }
            }

            if started.elapsed() > self.key_timeout {
                // Input lag is immediately noticeable, so this is a warning rather
                // than a debug line.
                log::warn!(
                    "plugin `{}` added {:?} of input latency (budget {:?})",
                    plugin.manifest.name,
                    started.elapsed(),
                    self.key_timeout
                );
            }
        }
        (KeyOutcome::Unhandled, out)
    }

    /// Record `RegisterCommand` and `RegisterKeybind` results.
    ///
    /// Kept out of the generic command drain because these must be applied before
    /// the keymap is built, not queued for the next frame.
    pub fn apply_registrations(&mut self, commands: Vec<Command>) {
        // Kept for callers that register on behalf of the most recent load. New code
        // should name the plugin: attributing by position is what filed one plugin's
        // keybinds under another's name.
        let index = self.plugins.len().saturating_sub(1);
        self.apply_registrations_for(index, commands);
    }

    /// Record registrations against the plugin that emitted them.
    fn apply_registrations_for(&mut self, index: usize, commands: Vec<Command>) {
        let Some(name) = self.plugins.get(index).map(|p| p.manifest.name.clone()) else {
            return;
        };

        for command in commands {
            match command {
                Command::RegisterCommand { name: command, .. } => {
                    // Namespaced so two plugins cannot collide on a common word
                    // like "toggle".
                    let qualified = format!("{name}.{command}");
                    if let Some(plugin) = self.plugins.get_mut(index) {
                        if !plugin.registered_commands.contains(&qualified) {
                            plugin.registered_commands.push(qualified);
                        }
                    }
                }
                Command::RegisterKeybind { chord, command } => {
                    let qualified = if command.contains('.') {
                        command
                    } else {
                        format!("{name}.{command}")
                    };
                    self.keybinds.insert(chord, qualified);
                }
                _ => {}
            }
        }
    }

    /// Keybinds plugins asked for, as chord -> command name.
    pub fn keybinds(&self) -> &HashMap<String, String> {
        &self.keybinds
    }
}

/// The event name used for manifest filtering. Must match the names documented
/// for `events` in `plugin.toml`.
pub fn event_name(event: &Event) -> &'static str {
    match event {
        Event::Startup => "startup",
        Event::ConfigReload => "config_reload",
        Event::Key(_) => "key",
        Event::PaneOutput { .. } => "pane_output",
        Event::TabSwitch { .. } => "tab_switch",
        Event::TitleChange { .. } => "title_change",
        Event::Bell { .. } => "bell",
        Event::PaneOpened { .. } => "pane_opened",
        Event::PaneClosed { .. } => "pane_closed",
        Event::Osc { .. } => "osc",
        Event::StatusBarRender => "status_bar_render",
        Event::StatusSegmentClick { .. } => "status_segment_click",
        Event::Command { .. } => "command",
        Event::InputLine { .. } => "input_line",
        // `Event` is non_exhaustive; an unnamed event is simply not deliverable
        // by name rather than a compile error in downstream builds.
        _ => "unknown",
    }
}

/// Scan `dirs` for plugin directories containing a `plugin.toml`.
///
/// Earlier directories win, so a user's own copy shadows an installed one of the
/// same name — the same precedence themes use.
pub fn discover(dirs: &[PathBuf]) -> Vec<Result<(PathBuf, Manifest), PluginError>> {
    let mut found = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        // Sorted so load order is deterministic; `read_dir` order is not.
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("plugin.toml");
            if !manifest_path.is_file() {
                continue;
            }

            let result = std::fs::read_to_string(&manifest_path)
                .map_err(|source| PluginError::Io {
                    path: manifest_path.clone(),
                    source,
                })
                .and_then(|src| {
                    Manifest::parse(&src).map_err(|source| PluginError::Manifest {
                        path: manifest_path.clone(),
                        source,
                    })
                });

            match result {
                Ok(manifest) => {
                    if seen.insert(manifest.name.clone()) {
                        found.push(Ok((path, manifest)));
                    } else {
                        log::debug!(
                            "ignoring shadowed plugin at {} (already loaded a `{}`)",
                            path.display(),
                            manifest.name
                        );
                    }
                }
                Err(e) => found.push(Err(e)),
            }
        }
    }
    found
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("failed to read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: ManifestError,
    },
    #[error("plugin `{plugin}`: entry file {path} does not exist")]
    MissingEntry { plugin: String, path: PathBuf },
    #[error("plugin `{plugin}` needs the `{runtime}` runtime, which this build does not include")]
    RuntimeUnavailable {
        plugin: String,
        runtime: &'static str,
    },
    #[error("plugin `{plugin}` failed to initialize: {message}")]
    Init { plugin: String, message: String },
    #[error("plugin `{plugin}` raised an error: {message}")]
    Runtime { plugin: String, message: String },
    #[error("plugin `{plugin}` exceeded its execution budget and was aborted")]
    Timeout { plugin: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_plugin_api::{Direction, KeyPress, PaneId, StatusSegment};

    /// A stub runtime so host behaviour can be tested without a real interpreter.
    struct Stub {
        /// Commands to return from each dispatch.
        commands: Vec<Command>,
        /// Fail every call.
        always_fails: bool,
        /// Claim key presses.
        claims_keys: bool,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl PluginRuntime for Stub {
        fn dispatch(&mut self, _event: &Event) -> Result<Vec<Command>, PluginError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.always_fails {
                return Err(PluginError::Runtime {
                    plugin: "stub".to_owned(),
                    message: "boom".to_owned(),
                });
            }
            Ok(self.commands.clone())
        }

        fn on_key(&mut self, _event: &Event) -> Result<(KeyOutcome, Vec<Command>), PluginError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.always_fails {
                return Err(PluginError::Runtime {
                    plugin: "stub".to_owned(),
                    message: "boom".to_owned(),
                });
            }
            let outcome = if self.claims_keys {
                KeyOutcome::Handled
            } else {
                KeyOutcome::Unhandled
            };
            Ok((outcome, self.commands.clone()))
        }

        fn runtime_name(&self) -> &'static str {
            "stub"
        }
    }

    fn manifest(name: &str, events: &[&str]) -> Manifest {
        Manifest {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            api_version: tuz_plugin_api::API_VERSION,
            runtime: Runtime::Lua,
            entry: "init.lua".to_owned(),
            description: String::new(),
            author: String::new(),
            homepage: String::new(),
            permissions: Vec::new(),
            events: events.iter().map(|s| (*s).to_owned()).collect(),
            config: Default::default(),
        }
    }

    fn host_with(stubs: Vec<(Manifest, Stub)>) -> Host {
        let mut host = Host::new(Duration::from_millis(250), Duration::from_millis(5));
        for (manifest, stub) in stubs {
            host.plugins.push(LoadedPlugin {
                manifest,
                directory: PathBuf::from("/nonexistent"),
                runtime: Box::new(stub),
                registered_commands: Vec::new(),
                status: Vec::new(),
                failures: 0,
                disabled: false,
            });
        }
        host
    }

    fn stub(commands: Vec<Command>) -> Stub {
        Stub {
            commands,
            always_fails: false,
            claims_keys: false,
            calls: Default::default(),
        }
    }

    #[test]
    fn dispatch_collects_commands_from_every_plugin() {
        let mut host = host_with(vec![
            (manifest("a", &[]), stub(vec![Command::NewTab])),
            (
                manifest("b", &[]),
                stub(vec![Command::Split {
                    direction: Direction::Right,
                }]),
            ),
        ]);

        let commands = host.dispatch(&Event::ConfigReload);
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], Command::NewTab);
    }

    #[test]
    fn a_plugin_only_receives_events_it_asked_for() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = stub(vec![]);
        s.calls = calls.clone();

        let mut host = host_with(vec![(manifest("picky", &["bell"]), s)]);

        host.dispatch(&Event::ConfigReload);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);

        host.dispatch(&Event::Bell { pane: PaneId(1) });
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn pane_output_is_withheld_without_the_permission() {
        // The privacy-sensitive path: asking for the event is not enough.
        let mut m = manifest("watcher", &["pane_output"]);
        m.permissions.clear();

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = stub(vec![]);
        s.calls = calls.clone();

        let mut host = host_with(vec![(m, s)]);
        host.dispatch(&Event::PaneOutput {
            pane: PaneId(1),
            text: "secret".to_owned(),
        });
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "output must not reach a plugin without read-output"
        );
    }

    #[test]
    fn pane_output_is_delivered_once_the_permission_is_granted() {
        let mut m = manifest("watcher", &["pane_output"]);
        m.permissions.push(tuz_plugin_api::Permission::ReadOutput);

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = stub(vec![]);
        s.calls = calls.clone();

        let mut host = host_with(vec![(m, s)]);
        host.dispatch(&Event::PaneOutput {
            pane: PaneId(1),
            text: "hello".to_owned(),
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    fn input_line() -> Event {
        Event::InputLine {
            pane: PaneId(1),
            line: "$ sudo -S hunter2".to_owned(),
            cursor_col: 17,
            at_line_end: true,
        }
    }

    #[test]
    fn the_input_line_is_withheld_without_the_permission() {
        // Same privacy-sensitive path as `pane_output`: asking for the event is not
        // enough, because this is what the user is typing.
        let mut m = manifest("suggester", &["input_line"]);
        m.permissions.clear();

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = stub(vec![]);
        s.calls = calls.clone();

        let mut host = host_with(vec![(m, s)]);
        host.dispatch(&input_line());
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "the typed line must not reach a plugin without read-input"
        );
    }

    #[test]
    fn the_input_line_is_delivered_once_the_permission_is_granted() {
        let mut m = manifest("suggester", &["input_line"]);
        m.permissions.push(tuz_plugin_api::Permission::ReadInput);

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut s = stub(vec![]);
        s.calls = calls.clone();

        let mut host = host_with(vec![(m, s)]);
        host.dispatch(&input_line());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn wants_reports_whether_any_enabled_plugin_asked_for_an_event() {
        // The app calls this to decide whether to take the terminal mutex at all, so
        // a wrong answer is either a missing feature or a lock taken for nobody.
        let mut granted = manifest("suggester", &["input_line"]);
        granted
            .permissions
            .push(tuz_plugin_api::Permission::ReadInput);
        let mut host = host_with(vec![(granted, stub(vec![]))]);
        assert!(host.wants("input_line"));

        // A plugin that failed its way out of the session must not keep the cost of
        // its events alive.
        host.plugins[0].disabled = true;
        assert!(!host.wants("input_line"));

        // The event alone is not enough, matching `wants_event`.
        let host = host_with(vec![(manifest("half", &["input_line"]), stub(vec![]))]);
        assert!(!host.wants("input_line"));
    }

    #[test]
    fn a_repeatedly_failing_plugin_is_disabled() {
        // Otherwise a broken plugin logs on every keystroke forever.
        let mut s = stub(vec![]);
        s.always_fails = true;
        let mut host = host_with(vec![(manifest("bad", &[]), s)]);

        for _ in 0..FAILURE_LIMIT {
            host.dispatch(&Event::ConfigReload);
        }
        assert!(host.plugins()[0].is_disabled());

        // And it stops being called.
        let before = host.plugins()[0].failures;
        host.dispatch(&Event::ConfigReload);
        assert_eq!(host.plugins()[0].failures, before);
    }

    #[test]
    fn one_failing_plugin_does_not_stop_the_others() {
        let mut bad = stub(vec![]);
        bad.always_fails = true;

        let mut host = host_with(vec![
            (manifest("bad", &[]), bad),
            (manifest("good", &[]), stub(vec![Command::NewTab])),
        ]);

        let commands = host.dispatch(&Event::ConfigReload);
        assert_eq!(commands, vec![Command::NewTab]);
    }

    #[test]
    fn a_successful_call_resets_the_failure_count() {
        let mut host = host_with(vec![(manifest("flaky", &[]), stub(vec![]))]);
        host.plugins[0].failures = FAILURE_LIMIT - 1;

        host.dispatch(&Event::ConfigReload);
        assert_eq!(host.plugins()[0].failures, 0);
        assert!(!host.plugins()[0].is_disabled());
    }

    #[test]
    fn key_dispatch_stops_at_the_first_plugin_that_claims_the_key() {
        let mut claimer = stub(vec![]);
        claimer.claims_keys = true;

        let second_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut second = stub(vec![]);
        second.calls = second_calls.clone();

        let mut host = host_with(vec![
            (manifest("first", &["key"]), claimer),
            (manifest("second", &["key"]), second),
        ]);

        let (outcome, _) = host.on_key(&Event::Key(KeyPress {
            chord: "ctrl+shift+p".to_owned(),
            modifiers: Default::default(),
        }));

        assert_eq!(outcome, KeyOutcome::Handled);
        assert_eq!(
            second_calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "a claimed key must not reach later plugins"
        );
    }

    #[test]
    fn an_unclaimed_key_falls_through_to_the_terminal() {
        let mut host = host_with(vec![(manifest("passive", &["key"]), stub(vec![]))]);
        let (outcome, _) = host.on_key(&Event::Key(KeyPress {
            chord: "a".to_owned(),
            modifiers: Default::default(),
        }));
        assert_eq!(outcome, KeyOutcome::Unhandled);
    }

    #[test]
    fn registered_commands_are_namespaced_by_plugin() {
        // Two plugins both registering "toggle" must not collide.
        let mut host = host_with(vec![(manifest("statusbar", &[]), stub(vec![]))]);
        host.apply_registrations(vec![Command::RegisterCommand {
            name: "toggle".to_owned(),
            description: String::new(),
        }]);

        assert_eq!(host.command_names(), vec!["statusbar.toggle".to_owned()]);
    }

    #[test]
    fn registering_the_same_command_twice_is_idempotent() {
        let mut host = host_with(vec![(manifest("p", &[]), stub(vec![]))]);
        let register = || Command::RegisterCommand {
            name: "go".to_owned(),
            description: String::new(),
        };
        host.apply_registrations(vec![register(), register()]);
        assert_eq!(host.command_names().len(), 1);
    }

    #[test]
    fn keybinds_are_namespaced_unless_already_qualified() {
        let mut host = host_with(vec![(manifest("mine", &[]), stub(vec![]))]);
        host.apply_registrations(vec![
            Command::RegisterKeybind {
                chord: "ctrl+shift+1".to_owned(),
                command: "go".to_owned(),
            },
            Command::RegisterKeybind {
                chord: "ctrl+shift+2".to_owned(),
                command: "other.go".to_owned(),
            },
        ]);

        assert_eq!(host.keybinds()["ctrl+shift+1"], "mine.go");
        assert_eq!(
            host.keybinds()["ctrl+shift+2"],
            "other.go",
            "an already-qualified name must be left alone"
        );
    }

    #[test]
    fn status_segments_are_captured_from_dispatch() {
        let segments = vec![StatusSegment {
            id: None,
            text: "cpu 4%".to_owned(),
            foreground: None,
            background: None,
        }];
        let mut host = host_with(vec![(
            manifest("bar", &[]),
            stub(vec![Command::SetStatusSegments {
                segments: segments.clone(),
            }]),
        )]);

        host.dispatch(&Event::StatusBarRender);
        assert_eq!(host.status_segments(), segments);
    }

    #[test]
    fn a_disabled_plugins_status_segments_disappear() {
        let mut host = host_with(vec![(
            manifest("bar", &[]),
            stub(vec![Command::SetStatusSegments {
                segments: vec![StatusSegment {
                    id: None,
                    text: "x".to_owned(),
                    foreground: None,
                    background: None,
                }],
            }]),
        )]);
        host.dispatch(&Event::StatusBarRender);
        assert_eq!(host.status_segments().len(), 1);

        host.plugins[0].disabled = true;
        assert!(host.status_segments().is_empty());
    }

    #[test]
    fn event_names_cover_every_named_variant() {
        // These strings are the manifest's `events` vocabulary, so a variant that
        // falls through to "unknown" would be silently undeliverable.
        let events = [
            Event::Startup,
            Event::ConfigReload,
            Event::Key(KeyPress {
                chord: "a".to_owned(),
                modifiers: Default::default(),
            }),
            Event::PaneOutput {
                pane: PaneId(1),
                text: String::new(),
            },
            Event::TabSwitch { index: 0 },
            Event::TitleChange {
                pane: PaneId(1),
                title: String::new(),
            },
            Event::Bell { pane: PaneId(1) },
            Event::PaneOpened { pane: PaneId(1) },
            Event::PaneClosed { pane: PaneId(1) },
            Event::Osc {
                pane: PaneId(1),
                code: 0,
                payload: String::new(),
            },
            Event::StatusBarRender,
            Event::Command {
                name: String::new(),
                args: vec![],
            },
        ];
        for event in events {
            assert_ne!(event_name(&event), "unknown", "{event:?} has no name");
        }
    }

    // --- discovery --------------------------------------------------------

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p =
                std::env::temp_dir().join(format!("tuz-plugin-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }

        /// Create a plugin directory with a manifest and an entry file.
        fn plugin(&self, name: &str, manifest: &str, entry: Option<&str>) -> PathBuf {
            let dir = self.0.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("plugin.toml"), manifest).unwrap();
            if let Some(entry) = entry {
                // Must be a *valid* plugin: the Lua runtime requires the entry file
                // to return a handler table, so a bare comment fails to load.
                std::fs::write(dir.join(entry), "return {}\n").unwrap();
            }
            dir
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn valid_manifest(name: &str) -> String {
        format!(
            r#"
name = "{name}"
version = "0.1.0"
api_version = {}
runtime = "lua"
entry = "init.lua"
"#,
            tuz_plugin_api::API_VERSION
        )
    }

    #[test]
    fn discovery_finds_plugins_with_a_manifest() {
        let dir = TempDir::new("discover");
        dir.plugin("alpha", &valid_manifest("alpha"), Some("init.lua"));
        dir.plugin("beta", &valid_manifest("beta"), Some("init.lua"));
        // A directory without a manifest is not a plugin.
        std::fs::create_dir_all(dir.0.join("not-a-plugin")).unwrap();

        let found = discover(std::slice::from_ref(&dir.0));
        assert_eq!(found.len(), 2);
        let names: Vec<String> = found
            .iter()
            .filter_map(|r| r.as_ref().ok().map(|(_, m)| m.name.clone()))
            .collect();
        assert_eq!(names, vec!["alpha", "beta"], "order must be deterministic");
    }

    #[test]
    fn a_malformed_manifest_is_reported_not_skipped_silently() {
        let dir = TempDir::new("malformed");
        dir.plugin("broken", "this is not toml {{{", Some("init.lua"));

        let found = discover(std::slice::from_ref(&dir.0));
        assert_eq!(found.len(), 1);
        assert!(found[0].is_err(), "a broken manifest should surface");
    }

    #[test]
    fn an_earlier_directory_shadows_a_later_one_of_the_same_name() {
        let user = TempDir::new("shadow-user");
        let installed = TempDir::new("shadow-installed");
        user.plugin("dup", &valid_manifest("dup"), Some("init.lua"));
        installed.plugin("dup", &valid_manifest("dup"), Some("init.lua"));

        let found = discover(&[user.0.clone(), installed.0.clone()]);
        assert_eq!(found.len(), 1, "only the shadowing copy should load");
        let (path, _) = found[0].as_ref().unwrap();
        assert!(path.starts_with(&user.0));
    }

    #[test]
    fn a_missing_entry_file_fails_the_load_with_a_clear_error() {
        let dir = TempDir::new("no-entry");
        // Manifest present, entry file absent.
        dir.plugin("headless", &valid_manifest("headless"), None);

        let mut host = Host::new(Duration::from_millis(250), Duration::from_millis(5));
        let errors = host.load_all(
            std::slice::from_ref(&dir.0),
            &tuz_config::Plugins::default(),
        );

        assert_eq!(errors.len(), 1);
        assert!(
            matches!(errors[0], PluginError::MissingEntry { .. }),
            "got {:?}",
            errors[0]
        );
        assert!(host.is_empty());
    }

    #[test]
    fn the_load_allowlist_and_disable_list_are_honored() {
        let dir = TempDir::new("filters");
        dir.plugin("wanted", &valid_manifest("wanted"), Some("init.lua"));
        dir.plugin("unwanted", &valid_manifest("unwanted"), Some("init.lua"));

        let cfg = tuz_config::Plugins {
            load: vec!["wanted".to_owned()],
            ..Default::default()
        };
        let mut host = Host::new(Duration::from_millis(250), Duration::from_millis(5));
        host.load_all(std::slice::from_ref(&dir.0), &cfg);
        assert_eq!(host.plugins().len(), 1);
        assert_eq!(host.plugins()[0].name(), "wanted");

        // `disable` wins even over an explicit `load`.
        let cfg = tuz_config::Plugins {
            load: vec!["wanted".to_owned()],
            disable: vec!["wanted".to_owned()],
            ..Default::default()
        };
        let mut host = Host::new(Duration::from_millis(250), Duration::from_millis(5));
        host.load_all(std::slice::from_ref(&dir.0), &cfg);
        assert!(host.is_empty());
    }

    #[test]
    fn discovery_of_a_nonexistent_directory_is_not_an_error() {
        // A user with no plugins directory is the normal case.
        assert!(discover(&[PathBuf::from("/nonexistent/tuz-plugins")]).is_empty());
    }
    /// Two plugins must not have their registrations mixed up.
    ///
    /// They were: `Startup` was broadcast on every load, so an earlier plugin's
    /// `on_startup` ran again each time a later one loaded, and the commands it
    /// returned were credited to whichever plugin was last in the list.
    #[test]
    fn each_plugins_registrations_are_filed_under_its_own_name() {
        let dir = std::env::temp_dir().join(format!("tuz-two-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        for (name, chord) in [("alpha", "ctrl+shift+1"), ("omega", "ctrl+shift+2")] {
            let plugin = dir.join(name);
            std::fs::create_dir_all(&plugin).unwrap();
            std::fs::write(
                plugin.join("plugin.toml"),
                format!(
                    "name = \"{name}\"\nversion = \"0.1.0\"\napi_version = {}\n\
                     runtime = \"lua\"\nentry = \"init.lua\"\n",
                    tuz_plugin_api::API_VERSION
                ),
            )
            .unwrap();
            std::fs::write(
                plugin.join("init.lua"),
                format!(
                    "local M = {{}}\n\
                     function M.on_startup(ctx)\n\
                       ctx.register_command(\"go\", \"\")\n\
                       ctx.register_keybind(\"{chord}\", \"go\")\n\
                     end\n\
                     return M\n"
                ),
            )
            .unwrap();
        }

        let mut host = Host::disabled();
        let errors = host.load_all(std::slice::from_ref(&dir), &tuz_config::Plugins::default());
        assert!(errors.is_empty(), "{errors:?}");

        let binds = host.keybinds();
        assert_eq!(
            binds.get("ctrl+shift+1").map(String::as_str),
            Some("alpha.go")
        );
        assert_eq!(
            binds.get("ctrl+shift+2").map(String::as_str),
            Some("omega.go")
        );

        let names = host.command_names();
        assert!(names.contains(&"alpha.go".to_owned()), "{names:?}");
        assert!(names.contains(&"omega.go".to_owned()), "{names:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
