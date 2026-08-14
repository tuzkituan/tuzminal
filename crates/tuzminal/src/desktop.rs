//! Registering the terminal with the desktop environment.
//!
//! `cargo install` puts a binary on `PATH` and nothing else, so the application never
//! appears in an app grid and the compositor has no icon or name for its window —
//! GNOME labels it "Unknown". Two small files fix that, and writing them is a thing
//! the binary can do for itself rather than a step buried in a README.
//!
//! Freedesktop only: macOS wants a bundle and Windows a shortcut, and neither is
//! served by dropping files in `~/.local/share`.

use std::path::{Path, PathBuf};

/// Must match [`crate::app::APP_ID`], or the window and the entry stay unpaired.
const ENTRY_NAME: &str = "tuzminal";

/// The icon, as scalable SVG. The same mark `appicon.rs` generates for the window.
const ICON: &str = include_str!("../../../assets/tuzminal.svg");

/// What was written, for reporting.
pub struct Installed {
    pub entry: PathBuf,
    pub icon: PathBuf,
}

/// Write the desktop entry and icon under `$XDG_DATA_HOME`.
///
/// `exec` is embedded as an absolute path rather than the bare name: a desktop entry
/// is launched with the session's environment, and `~/.cargo/bin` is on the shell's
/// `PATH` but usually not the session's — so `Exec=tuzminal` would produce an entry
/// that appears in the menu and fails to start.
pub fn install(data_home: &Path, exec: &Path) -> std::io::Result<Installed> {
    let apps = data_home.join("applications");
    let icons = data_home.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&apps)?;
    std::fs::create_dir_all(&icons)?;

    let entry = apps.join(format!("{ENTRY_NAME}.desktop"));
    std::fs::write(&entry, entry_contents(exec))?;

    let icon = icons.join(format!("{ENTRY_NAME}.svg"));
    std::fs::write(&icon, ICON)?;

    Ok(Installed { entry, icon })
}

fn entry_contents(exec: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Tuzminal\n\
         GenericName=Terminal\n\
         Comment=A GPU-accelerated terminal emulator\n\
         Exec={exec} %F\n\
         Icon={ENTRY_NAME}\n\
         Terminal=false\n\
         Categories=System;TerminalEmulator;\n\
         Keywords=shell;prompt;command;commandline;cmd;\n\
         StartupNotify=true\n\
         StartupWMClass={ENTRY_NAME}\n",
        exec = exec.display()
    )
}

/// Where the desktop files belong, honouring `$XDG_DATA_HOME`.
pub fn data_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_DATA_HOME") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return Some(dir);
        }
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tuz-desktop-{tag}-{}", std::process::id()));
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
    fn both_files_land_where_the_spec_says() {
        let tmp = TempDir::new("paths");
        let out = install(&tmp.0, Path::new("/home/me/.cargo/bin/tuzminal")).unwrap();

        assert_eq!(out.entry, tmp.0.join("applications/tuzminal.desktop"));
        assert_eq!(
            out.icon,
            tmp.0.join("icons/hicolor/scalable/apps/tuzminal.svg")
        );
        assert!(out.entry.is_file() && out.icon.is_file());
    }

    #[test]
    fn exec_is_absolute_so_the_launcher_can_find_it() {
        // A desktop entry runs with the session's environment, which usually has no
        // `~/.cargo/bin`. `Exec=tuzminal` gives a menu item that does nothing.
        let contents = entry_contents(Path::new("/home/me/.cargo/bin/tuzminal"));
        assert!(
            contents.contains("Exec=/home/me/.cargo/bin/tuzminal"),
            "{contents}"
        );
    }

    #[test]
    fn the_window_class_matches_the_entry_name() {
        // This pairing is what stops the compositor calling the window "Unknown".
        let contents = entry_contents(Path::new("/usr/bin/tuzminal"));
        assert!(contents.contains(&format!("StartupWMClass={ENTRY_NAME}")));
        assert_eq!(ENTRY_NAME, crate::app::APP_ID);
    }

    #[test]
    fn the_icon_written_is_a_real_svg() {
        let tmp = TempDir::new("icon");
        let out = install(&tmp.0, Path::new("/usr/bin/tuzminal")).unwrap();
        let svg = std::fs::read_to_string(out.icon).unwrap();
        assert!(svg.contains("<svg"), "not an svg: {svg:.60}");
    }

    #[test]
    fn installing_twice_overwrites_rather_than_failing() {
        // Re-running after an upgrade must refresh the path in `Exec`.
        let tmp = TempDir::new("twice");
        install(&tmp.0, Path::new("/old/tuzminal")).unwrap();
        let out = install(&tmp.0, Path::new("/new/tuzminal")).unwrap();
        let contents = std::fs::read_to_string(out.entry).unwrap();
        assert!(contents.contains("Exec=/new/tuzminal"), "{contents}");
    }

    #[test]
    fn xdg_data_home_wins_when_it_is_set_to_something() {
        // An empty value means unset, per the spec, and must not produce a relative
        // path that writes into the working directory.
        let previous = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("XDG_DATA_HOME", "");
        assert!(data_home().is_some_and(|p| p.is_absolute()));

        std::env::set_var("XDG_DATA_HOME", "/custom/share");
        assert_eq!(data_home(), Some(PathBuf::from("/custom/share")));

        match previous {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}
