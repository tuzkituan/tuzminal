//! Filesystem watching for live config reload.
//!
//! Two details make this reliable in practice:
//!
//! - **Debouncing.** Editors do not write files once. Vim writes a backup,
//!   renames, and truncates; many editors write-then-chmod. Without a debounce
//!   window a single `:w` triggers several reloads, at least one of which reads
//!   a half-written file.
//! - **Watching the directory, not the file.** An atomic save replaces the inode
//!   `config.toml` points at, so a file watch goes deaf after the first save.
//!   Watching the parent directory survives rename-based writes.

use crossbeam_channel::{Receiver, Sender};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Time to wait for filesystem activity to settle before reloading.
const DEBOUNCE: Duration = Duration::from_millis(100);

/// Sent when watched files have settled after a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigEvent {
    /// Something under a watched directory changed. The receiver should reload;
    /// this carries no payload because the debouncer coalesces unrelated edits.
    Changed,
}

/// Watches the config directory and emits debounced [`ConfigEvent`]s.
///
/// The watcher and its debounce thread live as long as this value; dropping it
/// stops watching.
pub struct ConfigWatcher {
    _watcher: RecommendedWatcher,
    rx: Receiver<ConfigEvent>,
    /// Kept so the debounce thread's `recv` fails and the thread exits on drop.
    _shutdown: Sender<()>,
}

impl ConfigWatcher {
    /// Start watching `dirs` (config dir, theme dirs, plugin dirs).
    ///
    /// Directories that do not exist are skipped rather than failing — a user
    /// with no `themes/` directory is a normal case, not an error.
    pub fn new(dirs: &[PathBuf]) -> Result<Self, WatchError> {
        let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<notify::Event>();
        let (out_tx, out_rx) = crossbeam_channel::unbounded::<ConfigEvent>();
        let (shutdown_tx, shutdown_rx) = crossbeam_channel::bounded::<()>(0);

        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    // A closed receiver means the ConfigWatcher was dropped;
                    // there is nothing useful to do but ignore it.
                    let _ = raw_tx.send(event);
                }
                Err(e) => log::warn!("config watch error: {e}"),
            }
        })
        .map_err(WatchError::Init)?;

        let mut watched = 0usize;
        for dir in dirs {
            if !dir.is_dir() {
                log::debug!("not watching {} (does not exist)", dir.display());
                continue;
            }
            match watcher.watch(dir, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched += 1;
                    log::debug!("watching {}", dir.display());
                }
                Err(e) => log::warn!("cannot watch {}: {e}", dir.display()),
            }
        }

        if watched == 0 {
            // Not fatal: the app still runs, it just cannot hot-reload. Manual
            // reload via keybind continues to work.
            log::info!("no config directories exist yet; live reload is inactive");
        }

        std::thread::Builder::new()
            .name("tuz-config-debounce".into())
            .spawn(move || debounce_loop(raw_rx, out_tx, shutdown_rx))
            .map_err(WatchError::Thread)?;

        Ok(Self {
            _watcher: watcher,
            rx: out_rx,
            _shutdown: shutdown_tx,
        })
    }

    /// Channel of debounced change events. Suitable for `select!` or polling.
    pub fn receiver(&self) -> &Receiver<ConfigEvent> {
        &self.rx
    }

    /// Non-blocking drain. Returns true when at least one change is pending,
    /// collapsing several queued changes into a single reload.
    pub fn poll(&self) -> bool {
        let mut changed = false;
        while self.rx.try_recv().is_ok() {
            changed = true;
        }
        changed
    }
}

/// Collapse a burst of filesystem events into one notification.
///
/// After the first relevant event, keep extending the deadline until `DEBOUNCE`
/// passes with no further activity, then emit once.
fn debounce_loop(raw: Receiver<notify::Event>, out: Sender<ConfigEvent>, shutdown: Receiver<()>) {
    loop {
        // Block until either a first event arrives or the owner drops.
        let first = crossbeam_channel::select! {
            recv(raw) -> ev => match ev {
                Ok(ev) => ev,
                Err(_) => return, // watcher gone
            },
            recv(shutdown) -> _ => return, // ConfigWatcher dropped
        };

        if !is_relevant(&first) {
            continue;
        }

        // Coalesce: swallow everything that arrives within the quiet window.
        loop {
            match raw.recv_timeout(DEBOUNCE) {
                Ok(_) => continue,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        }

        if out.send(ConfigEvent::Changed).is_err() {
            return; // receiver gone
        }
    }
}

/// Filter out events that cannot change effective configuration.
///
/// Editors litter the directory with swap and backup files; reloading on those
/// is pure waste. Access-only events are ignored for the same reason.
fn is_relevant(event: &notify::Event) -> bool {
    use notify::EventKind;

    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    // A rename or removal may carry no surviving path but still matters.
    if event.paths.is_empty() {
        return true;
    }
    event.paths.iter().any(|p| is_config_path(p))
}

/// True for files that participate in configuration.
fn is_config_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    // Editor scratch files: vim `.swp`/`~`, emacs `#file#` and `.#file`,
    // and the `.tmp`/`.new` names used by atomic-write helpers.
    if name.starts_with('.') && name.ends_with(".swp") {
        return false;
    }
    if name.ends_with('~') || name.starts_with(".#") || name.starts_with('#') {
        return false;
    }

    match path.extension().and_then(|e| e.to_str()) {
        Some("toml") | Some("lua") | Some("wasm") => true,
        // Extensionless or unknown: only interesting if it is the config file
        // itself mid-atomic-write.
        _ => name == "config.toml",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("failed to initialize the filesystem watcher")]
    Init(#[source] notify::Error),
    #[error("failed to spawn the config debounce thread")]
    Thread(#[source] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_asset_files_are_relevant() {
        assert!(is_config_path(Path::new("/cfg/config.toml")));
        assert!(is_config_path(Path::new("/cfg/themes/dracula.toml")));
        assert!(is_config_path(Path::new("/cfg/plugins/bar/init.lua")));
        assert!(is_config_path(Path::new("/cfg/plugins/bar/plugin.wasm")));
    }

    #[test]
    fn editor_scratch_files_are_ignored() {
        // Each of these is written during a normal `:w` or emacs save and would
        // otherwise cause a spurious reload of a partial file.
        for p in [
            "/cfg/.config.toml.swp",
            "/cfg/config.toml~",
            "/cfg/.#config.toml",
            "/cfg/#config.toml#",
            "/cfg/notes.txt",
        ] {
            assert!(!is_config_path(Path::new(p)), "{p} should be ignored");
        }
    }

    #[test]
    fn access_events_never_trigger_reload() {
        use notify::event::{AccessKind, EventKind};
        let ev = notify::Event {
            kind: EventKind::Access(AccessKind::Read),
            paths: vec![PathBuf::from("/cfg/config.toml")],
            attrs: Default::default(),
        };
        assert!(!is_relevant(&ev));
    }

    #[test]
    fn pathless_events_are_treated_as_relevant() {
        // Some backends report a rename with no usable path; assuming
        // "irrelevant" there would silently break reload after an atomic save.
        let ev = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![],
            attrs: Default::default(),
        };
        assert!(is_relevant(&ev));
    }

    #[test]
    fn missing_directories_do_not_prevent_construction() {
        let w = ConfigWatcher::new(&[PathBuf::from("/nonexistent/tuz-watch-test")])
            .expect("absent directories should be skipped, not fatal");
        assert!(!w.poll());
    }
}
