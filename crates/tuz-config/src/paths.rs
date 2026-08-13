//! Filesystem locations for config, themes and plugins.
//!
//! Two roots, deliberately separated so that reinstalling or wiping downloaded
//! content never touches hand-written config:
//!
//! - **config** (`$XDG_CONFIG_HOME/tuzminal`) — `config.toml`, user-authored
//!   `themes/` and `plugins/`.
//! - **data** (`$XDG_DATA_HOME/tuzminal`) — themes and plugins installed by the
//!   package manager, plus `plugins.lock`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    /// Precomputed search order: user config dir first, then installed data dir.
    theme_dirs: Vec<PathBuf>,
    plugin_dirs: Vec<PathBuf>,
}

impl Paths {
    /// Discover the standard platform directories.
    ///
    /// `TUZMINAL_CONFIG_DIR` and `TUZMINAL_DATA_DIR` override them, which is how
    /// the test suite and portable installs stay off the real user's files.
    pub fn discover() -> Result<Self, PathsError> {
        let config_dir = match std::env::var_os("TUZMINAL_CONFIG_DIR") {
            Some(p) => PathBuf::from(p),
            None => directories::ProjectDirs::from("", "", "tuzminal")
                .ok_or(PathsError::NoHomeDirectory)?
                .config_dir()
                .to_path_buf(),
        };
        let data_dir = match std::env::var_os("TUZMINAL_DATA_DIR") {
            Some(p) => PathBuf::from(p),
            None => directories::ProjectDirs::from("", "", "tuzminal")
                .ok_or(PathsError::NoHomeDirectory)?
                .data_dir()
                .to_path_buf(),
        };
        Ok(Self::new(config_dir, data_dir))
    }

    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        let theme_dirs = vec![config_dir.join("themes"), data_dir.join("themes")];
        let plugin_dirs = vec![config_dir.join("plugins"), data_dir.join("plugins")];
        Self {
            config_dir,
            data_dir,
            theme_dirs,
            plugin_dirs,
        }
    }

    /// Paths rooted at a nonexistent directory, so lookups miss and fall through
    /// to built-ins without touching the real filesystem.
    #[cfg(any(test, feature = "test-util"))]
    pub fn for_test() -> Self {
        Self::new(
            PathBuf::from("/nonexistent/tuzminal-test/config"),
            PathBuf::from("/nonexistent/tuzminal-test/data"),
        )
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn plugin_lock_file(&self) -> PathBuf {
        self.data_dir.join("plugins.lock")
    }

    /// Theme search order, highest precedence first.
    pub fn theme_dirs(&self) -> &[PathBuf] {
        &self.theme_dirs
    }
    /// Plugin search order, highest precedence first.
    pub fn plugin_dirs(&self) -> &[PathBuf] {
        &self.plugin_dirs
    }

    /// Create the config tree if absent. Never overwrites existing files.
    pub fn ensure_config_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.config_dir.join("themes"))?;
        std::fs::create_dir_all(self.config_dir.join("plugins"))?;
        Ok(())
    }

    /// Create the data tree if absent. Never overwrites existing files.
    pub fn ensure_data_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.data_dir.join("themes"))?;
        std::fs::create_dir_all(self.data_dir.join("plugins"))?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the user's home directory")]
    NoHomeDirectory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_config_dir_precedes_installed_data_dir() {
        let p = Paths::new(PathBuf::from("/cfg"), PathBuf::from("/data"));
        assert_eq!(
            p.theme_dirs(),
            [PathBuf::from("/cfg/themes"), PathBuf::from("/data/themes")]
        );
        assert_eq!(
            p.plugin_dirs(),
            [
                PathBuf::from("/cfg/plugins"),
                PathBuf::from("/data/plugins")
            ]
        );
    }

    #[test]
    fn well_known_files_sit_in_the_right_root() {
        let p = Paths::new(PathBuf::from("/cfg"), PathBuf::from("/data"));
        assert_eq!(p.config_file(), PathBuf::from("/cfg/config.toml"));
        // The lockfile tracks installed content, so it belongs in the data root.
        assert_eq!(p.plugin_lock_file(), PathBuf::from("/data/plugins.lock"));
    }
}
