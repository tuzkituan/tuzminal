//! The plugins that ship with the binary.
//!
//! Compiled in and written to disk on first launch, so a fresh install has working
//! plugins to look at rather than an empty Plugins page and a pointer to the docs.
//! They are ordinary plugins once written — visible on the page, toggleable, editable,
//! deletable — not a privileged built-in tier.
//!
//! Only ever written when the target directory does not already exist. Rewriting them
//! on every launch would silently discard the edits of anyone using them as a starting
//! point, which is exactly what they are for.

use std::path::Path;

/// A plugin's files: `(relative path, contents)`.
type Files = &'static [(&'static str, &'static str)];

/// Every bundled plugin, as `(name, files)`.
///
/// `include_str!` rather than a build script: two small Lua files do not justify one,
/// and this way the plugins in the repository and the ones installed are provably the
/// same text.
const BUNDLED: &[(&str, Files)] = &[
    (
        "clock",
        &[
            (
                "plugin.toml",
                include_str!("../../../plugins/clock/plugin.toml"),
            ),
            ("init.lua", include_str!("../../../plugins/clock/init.lua")),
        ],
    ),
    (
        "open-in-ide",
        &[
            (
                "plugin.toml",
                include_str!("../../../plugins/open-in-ide/plugin.toml"),
            ),
            (
                "init.lua",
                include_str!("../../../plugins/open-in-ide/init.lua"),
            ),
        ],
    ),
];

/// Write any bundled plugin that is not already installed.
///
/// Returns the names written. A plugin the user deleted stays deleted only until the
/// next launch — which is a real limitation, and the reason the Plugins page offers
/// disabling as well as deleting: disabling persists in config, deleting does not.
pub fn install_missing(plugins_dir: &Path) -> Vec<String> {
    let mut written = Vec::new();

    for (name, files) in BUNDLED {
        let dir = plugins_dir.join(name);
        if dir.exists() {
            continue;
        }
        if let Err(e) = write_plugin(&dir, files) {
            log::warn!("could not install the bundled `{name}` plugin: {e}");
            // Leave nothing half-written: a directory with a manifest and no entry
            // file loads as an error on every launch.
            let _ = std::fs::remove_dir_all(&dir);
            continue;
        }
        log::info!("installed the bundled `{name}` plugin");
        written.push((*name).to_owned());
    }
    written
}

fn write_plugin(dir: &Path, files: Files) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, contents) in files {
        std::fs::write(dir.join(name), contents)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tuz-bundled-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fresh_directory_gets_every_bundled_plugin() {
        let tmp = TempDir::new("fresh");
        let written = install_missing(&tmp.0);

        assert_eq!(written.len(), BUNDLED.len());
        for (name, files) in BUNDLED {
            for (file, _) in *files {
                assert!(
                    tmp.0.join(name).join(file).is_file(),
                    "{name}/{file} was not written"
                );
            }
        }
    }

    #[test]
    fn an_edited_plugin_is_left_alone() {
        // These are meant to be starting points. Rewriting them every launch would
        // silently discard the changes of anyone who took that invitation.
        let tmp = TempDir::new("edited");
        install_missing(&tmp.0);

        let entry = tmp.0.join("clock/init.lua");
        std::fs::write(&entry, b"-- mine now").unwrap();

        let written = install_missing(&tmp.0);
        assert!(written.is_empty(), "nothing should be rewritten");
        assert_eq!(std::fs::read(&entry).unwrap(), b"-- mine now");
    }

    #[test]
    fn what_is_written_is_what_the_repository_ships() {
        // The bundled copy and `plugins/` must not drift: the guide points readers at
        // `plugins/`, and the tests load from there.
        let tmp = TempDir::new("same");
        install_missing(&tmp.0);

        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        for (name, files) in BUNDLED {
            for (file, _) in *files {
                let installed = std::fs::read_to_string(tmp.0.join(name).join(file)).unwrap();
                let source = std::fs::read_to_string(repo.join(name).join(file)).unwrap();
                assert_eq!(installed, source, "{name}/{file} drifted");
            }
        }
    }

    #[test]
    fn every_bundled_manifest_parses_and_names_a_file_that_exists() {
        // A bundled plugin that fails to load would greet every new user with an
        // error, so this is checked at build time rather than at first launch.
        for (name, files) in BUNDLED {
            let manifest_text = files
                .iter()
                .find(|(f, _)| *f == "plugin.toml")
                .map(|(_, c)| *c)
                .unwrap_or_else(|| panic!("{name} has no plugin.toml"));

            let manifest = tuz_plugin_api::Manifest::parse(manifest_text)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(manifest.name, *name, "directory and manifest name disagree");
            assert!(
                files.iter().any(|(f, _)| *f == manifest.entry),
                "{name} declares entry `{}` which is not bundled",
                manifest.entry
            );
        }
    }
}
