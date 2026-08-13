//! Configuration for Tuzminal: schema, theme resolution and live reload.
//!
//! # Design
//!
//! The central guarantee is that **a broken config never breaks a running
//! terminal**. [`ConfigManager::load`] falls back to built-in defaults when the
//! file is missing or malformed, and [`ConfigManager::reload`] keeps the previous
//! good settings when a live edit fails to parse or validate — surfacing the
//! error for display instead of exiting. Editing a config file should never be
//! able to kill a terminal that has work in it.
//!
//! ```no_run
//! use tuz_config::{ConfigManager, Paths, ReloadOutcome};
//!
//! let paths = Paths::discover()?;
//! let mut mgr = ConfigManager::load(paths);
//! if let Some(err) = mgr.last_error() {
//!     eprintln!("using defaults: {err}");
//! }
//!
//! mgr.watch()?; // begin live reload
//!
//! // ... later, in the event loop:
//! if mgr.poll_changes() {
//!     match mgr.reload() {
//!         ReloadOutcome::Applied(actions) if actions.rebuild_fonts => { /* rebuild atlas */ }
//!         ReloadOutcome::Applied(_) => { /* redraw */ }
//!         ReloadOutcome::Unchanged => {}
//!         ReloadOutcome::Failed(e) => eprintln!("config error, keeping previous: {e}"),
//!     }
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod color;
pub mod paths;
pub mod save;
pub mod schema;
pub mod theme;
pub mod watcher;

pub use color::{ColorParseError, Rgba};
pub use paths::{Paths, PathsError};
pub use save::{save_config, SaveError};
pub use schema::{
    Config, Cursor, CursorShape, Font, GpuBackend, Padding, Performance, Plugins, PowerPreference,
    ReloadActions, Scrollback, Shell, ValidationError, Window, DEFAULT_KEYS,
};
pub use theme::{AnsiPalette, Theme, ThemeError, BUILTIN_THEMES, DEFAULT_THEME};
pub use watcher::{ConfigEvent, ConfigWatcher, WatchError};

use std::path::PathBuf;

/// A commented starter config, written by `tuzminal --init-config`.
pub const EXAMPLE_CONFIG: &str = include_str!("../config.example.toml");

/// Config plus its resolved theme — everything the app needs to render.
///
/// `Default` is the built-in config paired with the built-in theme, which is what
/// the app falls back to when config cannot be read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub config: Config,
    pub theme: Theme,
}

/// Why a load or reload failed.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("failed to read {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{path} is not valid TOML: {source}")]
    Syntax {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },

    #[error("invalid configuration:\n{}", errors.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n"))]
    Invalid { errors: Vec<ValidationError> },

    #[error(transparent)]
    Theme(#[from] ThemeError),
}

/// Result of a [`ConfigManager::reload`].
#[derive(Debug)]
pub enum ReloadOutcome {
    /// New settings are in effect; perform the described work.
    Applied(ReloadActions),
    /// The files changed but the effective configuration did not.
    Unchanged,
    /// Reload failed. Previous settings remain in effect.
    Failed(LoadError),
}

/// Owns the live configuration and applies updates to it.
pub struct ConfigManager {
    paths: Paths,
    settings: Settings,
    watcher: Option<ConfigWatcher>,
    /// Error from the most recent load attempt, for display in the UI. Cleared
    /// by a subsequent successful load.
    last_error: Option<String>,
}

impl ConfigManager {
    /// Load configuration, falling back to defaults on any failure.
    ///
    /// Never fails: a user whose config has a typo still gets a working
    /// terminal. Inspect [`last_error`](Self::last_error) to report the problem.
    pub fn load(paths: Paths) -> Self {
        let (settings, last_error) = match Self::read(&paths) {
            Ok(s) => (s, None),
            Err(e) => {
                log::warn!("falling back to default configuration: {e}");
                (Settings::default(), Some(e.to_string()))
            }
        };
        Self {
            paths,
            settings,
            watcher: None,
            last_error,
        }
    }

    /// Read and validate settings from disk without mutating anything.
    fn read(paths: &Paths) -> Result<Settings, LoadError> {
        let path = paths.config_file();

        let config = match std::fs::read_to_string(&path) {
            Ok(src) => {
                let config: Config = toml::from_str(&src).map_err(|source| LoadError::Syntax {
                    path: path.clone(),
                    source: Box::new(source),
                })?;
                config
                    .validate()
                    .map_err(|errors| LoadError::Invalid { errors })?;
                config
            }
            // No config file is the normal first-run state, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("no config at {}; using defaults", path.display());
                Config::default()
            }
            Err(source) => return Err(LoadError::Io { path, source }),
        };

        let theme = Theme::load(&config.theme, paths)?;
        Ok(Settings { config, theme })
    }

    /// Begin watching for changes so [`poll_changes`](Self::poll_changes) works.
    pub fn watch(&mut self) -> Result<(), WatchError> {
        let mut dirs = vec![self.paths.config_dir().to_path_buf()];
        dirs.extend(self.paths.theme_dirs().iter().cloned());
        dirs.extend(self.paths.plugin_dirs().iter().cloned());
        self.watcher = Some(ConfigWatcher::new(&dirs)?);
        Ok(())
    }

    /// True when a watched file changed since the last call.
    ///
    /// Cheap and non-blocking; safe to call every frame.
    pub fn poll_changes(&self) -> bool {
        self.watcher.as_ref().is_some_and(|w| w.poll())
    }

    /// Channel of debounced change events, for event loops that would rather
    /// wait than poll.
    pub fn change_receiver(&self) -> Option<&crossbeam_channel::Receiver<ConfigEvent>> {
        self.watcher.as_ref().map(|w| w.receiver())
    }

    /// Re-read from disk and swap in the new settings if they are valid.
    ///
    /// On failure the previous settings are preserved and the error is returned;
    /// the caller should surface it without terminating.
    pub fn reload(&mut self) -> ReloadOutcome {
        match Self::read(&self.paths) {
            Ok(new) => {
                self.last_error = None;

                let mut actions = self.settings.config.diff(&new.config);
                // The theme file itself may have been edited while its name
                // stayed the same, which the config diff cannot see.
                if self.settings.theme != new.theme {
                    actions.reload_theme = true;
                    actions.redraw = true;
                }

                if actions.is_empty() {
                    return ReloadOutcome::Unchanged;
                }
                self.settings = new;
                ReloadOutcome::Applied(actions)
            }
            Err(e) => {
                log::warn!("config reload failed, keeping previous settings: {e}");
                self.last_error = Some(e.to_string());
                ReloadOutcome::Failed(e)
            }
        }
    }

    /// Apply an in-memory change, e.g. the `increase_font_size` action.
    ///
    /// Returns the work needed. The change is transient: the next reload from
    /// disk overwrites it, which is the desired behavior for runtime tweaks.
    pub fn modify(&mut self, f: impl FnOnce(&mut Config)) -> ReloadActions {
        let old = self.settings.config.clone();
        f(&mut self.settings.config);

        // A runtime tweak must not be able to produce an invalid state; roll
        // back rather than propagating something the renderer cannot handle.
        if let Err(errors) = self.settings.config.validate() {
            log::warn!("rejecting invalid runtime config change: {errors:?}");
            self.settings.config = old;
            return ReloadActions::default();
        }
        old.diff(&self.settings.config)
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    pub fn config(&self) -> &Config {
        &self.settings.config
    }
    pub fn theme(&self) -> &Theme {
        &self.settings.theme
    }
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Error from the most recent load attempt, if it failed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Write changed settings back to `config.toml`.
    ///
    /// `baseline` is what the file is believed to already contain — normally the
    /// config as it was when the settings panel opened. Only differing keys are
    /// written, and comments and formatting are preserved; see [`save`] for why that
    /// matters.
    pub fn save(&self, baseline: &Config) -> Result<PathBuf, SaveError> {
        save::save_config(&self.paths.config_file(), self.config(), baseline)
    }

    /// Write [`EXAMPLE_CONFIG`] to the config path.
    ///
    /// Refuses to clobber an existing file — losing a hand-tuned config to a
    /// stray `--init-config` would be unforgivable.
    pub fn write_example_config(&self) -> Result<PathBuf, std::io::Error> {
        self.paths.ensure_config_dirs()?;
        let path = self.paths.config_file();
        if path.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            ));
        }
        std::fs::write(&path, EXAMPLE_CONFIG)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp directory that cleans itself up, so tests never touch real config.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            // Thread id keeps concurrently running tests from colliding without
            // pulling in a temp-file dependency.
            let unique = format!(
                "tuz-cfg-test-{tag}-{:?}-{}",
                std::thread::current().id(),
                std::process::id()
            )
            .replace(['(', ')', ' '], "");
            let p = std::env::temp_dir().join(unique);
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn manager_with(tag: &str, config_toml: Option<&str>) -> (TempDir, ConfigManager) {
        let dir = TempDir::new(tag);
        let cfg = dir.path().join("config");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        if let Some(src) = config_toml {
            std::fs::write(cfg.join("config.toml"), src).unwrap();
        }
        let paths = Paths::new(cfg, data);
        (dir, ConfigManager::load(paths))
    }

    #[test]
    fn missing_config_file_is_not_an_error() {
        let (_d, mgr) = manager_with("missing", None);
        assert_eq!(mgr.config(), &Config::default());
        assert!(
            mgr.last_error().is_none(),
            "a first run with no config file is normal"
        );
    }

    #[test]
    fn valid_config_is_applied() {
        let (_d, mgr) = manager_with(
            "valid",
            Some("theme = \"tuz-light\"\n[font]\nsize = 15.0\n"),
        );
        assert_eq!(mgr.config().font.size, 15.0);
        assert_eq!(mgr.theme().name, "tuz-light");
        assert!(mgr.last_error().is_none());
    }

    #[test]
    fn malformed_config_falls_back_to_defaults_and_reports() {
        let (_d, mgr) = manager_with("broken", Some("this is not toml {{{"));
        assert_eq!(mgr.config(), &Config::default());
        let err = mgr.last_error().expect("should report the syntax error");
        assert!(err.contains("not valid TOML"), "{err}");
    }

    #[test]
    fn semantically_invalid_config_falls_back_and_lists_fields() {
        let (_d, mgr) = manager_with("invalid", Some("[window]\nopacity = 3.0\n"));
        assert_eq!(mgr.config(), &Config::default());
        let err = mgr.last_error().unwrap();
        assert!(err.contains("window.opacity"), "{err}");
    }

    #[test]
    fn unknown_theme_falls_back_rather_than_launching_unstyled() {
        let (_d, mgr) = manager_with("badtheme", Some("theme = \"no-such-theme\"\n"));
        assert_eq!(mgr.theme().name, DEFAULT_THEME);
        assert!(mgr.last_error().unwrap().contains("not found"));
    }

    #[test]
    fn reload_picks_up_edits_and_reports_minimal_work() {
        let (dir, mut mgr) = manager_with("reload", Some("[font]\nsize = 12.0\n"));
        let cfg_file = dir.path().join("config").join("config.toml");

        std::fs::write(&cfg_file, "[font]\nsize = 18.0\n").unwrap();
        match mgr.reload() {
            ReloadOutcome::Applied(a) => {
                assert!(a.rebuild_fonts);
                assert!(a.relayout);
                assert!(!a.reload_plugins);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(mgr.config().font.size, 18.0);
    }

    #[test]
    fn reload_of_an_unchanged_file_is_a_no_op() {
        let (_d, mut mgr) = manager_with("noop", Some("[font]\nsize = 12.0\n"));
        assert!(matches!(mgr.reload(), ReloadOutcome::Unchanged));
    }

    #[test]
    fn failed_reload_preserves_the_last_good_settings() {
        // The core promise: a typo saved into a live config must not disturb the
        // running terminal.
        let (dir, mut mgr) = manager_with("keepgood", Some("[font]\nsize = 16.0\n"));
        let cfg_file = dir.path().join("config").join("config.toml");

        std::fs::write(&cfg_file, "[font]\nsize = \"enormous\"\n").unwrap();
        assert!(matches!(mgr.reload(), ReloadOutcome::Failed(_)));
        assert_eq!(mgr.config().font.size, 16.0, "must keep the working value");
        assert!(mgr.last_error().is_some());

        // Fixing the file clears the error.
        std::fs::write(&cfg_file, "[font]\nsize = 17.0\n").unwrap();
        assert!(matches!(mgr.reload(), ReloadOutcome::Applied(_)));
        assert_eq!(mgr.config().font.size, 17.0);
        assert!(mgr.last_error().is_none());
    }

    #[test]
    fn reload_detects_an_edited_theme_file_under_an_unchanged_name() {
        let dir = TempDir::new("themeedit");
        let cfg = dir.path().join("config");
        std::fs::create_dir_all(cfg.join("themes")).unwrap();
        std::fs::write(cfg.join("config.toml"), "theme = \"mine\"\n").unwrap();

        let theme_src = |bg: &str| {
            format!(
                r##"
background = "{bg}"
foreground = "#ffffff"
[normal]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#ffffff"
[bright]
black = "#111111"
red = "#ff1111"
green = "#11ff11"
yellow = "#ffff11"
blue = "#1111ff"
magenta = "#ff11ff"
cyan = "#11ffff"
white = "#ffffff"
"##
            )
        };
        let theme_file = cfg.join("themes").join("mine.toml");
        std::fs::write(&theme_file, theme_src("#000000")).unwrap();

        let mut mgr = ConfigManager::load(Paths::new(cfg, dir.path().join("data")));
        assert_eq!(mgr.theme().background, Rgba::rgb(0, 0, 0));

        // Only the theme file changed — config.toml is byte-identical, so this
        // is exactly the case a config-only diff would miss.
        std::fs::write(&theme_file, theme_src("#123456")).unwrap();
        match mgr.reload() {
            ReloadOutcome::Applied(a) => assert!(a.reload_theme),
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(mgr.theme().background, Rgba::rgb(0x12, 0x34, 0x56));
    }

    #[test]
    fn user_theme_dir_shadows_a_builtin_of_the_same_name() {
        let dir = TempDir::new("shadow");
        let cfg = dir.path().join("config");
        std::fs::create_dir_all(cfg.join("themes")).unwrap();
        std::fs::write(
            cfg.join("themes").join(format!("{DEFAULT_THEME}.toml")),
            r##"
background = "#abcdef"
foreground = "#000000"
[normal]
black = "#000000"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
blue = "#0000ff"
magenta = "#ff00ff"
cyan = "#00ffff"
white = "#ffffff"
[bright]
black = "#111111"
red = "#ff1111"
green = "#11ff11"
yellow = "#ffff11"
blue = "#1111ff"
magenta = "#ff11ff"
cyan = "#11ffff"
white = "#ffffff"
"##,
        )
        .unwrap();

        let mgr = ConfigManager::load(Paths::new(cfg, dir.path().join("data")));
        assert_eq!(
            mgr.theme().background,
            Rgba::parse("#abcdef").unwrap(),
            "the user's file must win over the bundled theme"
        );
    }

    #[test]
    fn modify_applies_a_runtime_change() {
        let (_d, mut mgr) = manager_with("modify", None);
        let before = mgr.config().font.size;
        let actions = mgr.modify(|c| c.font.size += 2.0);
        assert!(actions.rebuild_fonts);
        assert_eq!(mgr.config().font.size, before + 2.0);
    }

    #[test]
    fn modify_rolls_back_a_change_that_would_be_invalid() {
        // Guards against e.g. decrease_font_size being held down until the size
        // reaches zero and the renderer divides by it.
        let (_d, mut mgr) = manager_with("rollback", None);
        let before = mgr.config().font.size;
        let actions = mgr.modify(|c| c.font.size = -5.0);
        assert!(actions.is_empty());
        assert_eq!(mgr.config().font.size, before);
    }

    #[test]
    fn write_example_config_refuses_to_overwrite() {
        let (_d, mgr) = manager_with("example", None);
        let path = mgr.write_example_config().expect("first write should work");
        assert!(path.exists());
        let err = mgr
            .write_example_config()
            .expect_err("second write must not clobber");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn the_bundled_example_config_is_itself_valid() {
        // Shipping an example that does not parse would be an embarrassing way
        // to greet a new user.
        let config: Config = toml::from_str(EXAMPLE_CONFIG)
            .expect("config.example.toml must parse against the current schema");
        config.validate().expect("example config must validate");
    }

    #[test]
    fn poll_changes_is_false_without_a_watcher() {
        let (_d, mgr) = manager_with("nowatch", None);
        assert!(!mgr.poll_changes());
    }
}
