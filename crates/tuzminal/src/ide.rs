//! "Open in…" buttons for whatever editors are actually installed.
//!
//! The list is fixed but the buttons are not: an entry only appears when its command
//! is on `PATH`, so a machine with only VS Code shows one button rather than a row of
//! things that would fail. Detection runs once at startup — `PATH` does not change
//! under a running process often enough to be worth re-scanning per frame.
//!
//! Opening goes through the focused shell rather than `Command::spawn`, for two
//! reasons: the editor inherits the shell's environment (which is where `PATH`,
//! `NVM_DIR` and the rest live), and the command is visible in scrollback, so it is
//! obvious what was run.

use std::path::{Path, PathBuf};

/// An editor we know how to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ide {
    /// Full name, for tooltips and messages.
    pub label: &'static str,
    /// A one- or two-letter monogram, which is what the button actually shows.
    ///
    /// A picture would be better and is not available: no font is guaranteed to carry
    /// an editor's logo, and drawing eight of them as geometry would be eight shapes
    /// to get subtly wrong. Initials are compact, unmistakable between the editors
    /// people actually have installed side by side, and render in the same font as
    /// everything else.
    pub icon: &'static str,
    /// The command to run.
    pub command: &'static str,
}

/// Editors worth offering, most common first.
///
/// Ordered rather than alphabetical because the buttons are laid out in this order
/// and the one you are most likely to want should be nearest the edge.
const KNOWN: &[Ide] = &[
    Ide {
        label: "VS Code",
        icon: "VS",
        command: "code",
    },
    Ide {
        label: "Cursor",
        icon: "Cu",
        command: "cursor",
    },
    Ide {
        label: "Windsurf",
        icon: "Wi",
        command: "windsurf",
    },
    Ide {
        label: "Zed",
        icon: "Ze",
        command: "zed",
    },
    Ide {
        label: "Sublime",
        icon: "Su",
        command: "subl",
    },
    Ide {
        label: "IntelliJ",
        icon: "IJ",
        command: "idea",
    },
    Ide {
        label: "WebStorm",
        icon: "WS",
        command: "webstorm",
    },
    Ide {
        label: "Neovim",
        icon: "Nv",
        command: "nvim",
    },
];

/// The subset of [`KNOWN`] whose command exists on `PATH`.
pub fn available() -> Vec<Ide> {
    let path = std::env::var_os("PATH");
    KNOWN
        .iter()
        .copied()
        .filter(|ide| path.as_deref().is_some_and(|p| on_path(p, ide.command)))
        .collect()
}

/// Whether `command` is an executable file in one of `path`'s directories.
///
/// Split out from [`available`] so the search can be tested without depending on what
/// happens to be installed on the machine running the tests.
fn on_path(path: &std::ffi::OsStr, command: &str) -> bool {
    std::env::split_paths(path).any(|dir| is_executable(&dir.join(command)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// The command that opens `target` in `ide`, ready to write to a PTY.
///
/// The path is quoted; the command name is not, because it is one of our own
/// constants rather than anything the filesystem supplied. Terminated with `\r`, the
/// carriage return a shell submits on.
pub fn open_command(ide: Ide, target: &Path) -> Vec<u8> {
    format!(
        "{} {}\r",
        ide.command,
        crate::explorer::shell_quote(&target.to_string_lossy())
    )
    .into_bytes()
}

/// Directory an "open in…" button should act on when nothing is selected.
pub fn fallback_target(cwd: Option<&Path>) -> PathBuf {
    cwd.map(Path::to_path_buf)
        .or_else(crate::proc::home)
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_quotes_its_path_but_not_itself() {
        let ide = Ide {
            label: "VS Code",
            icon: "VS",
            command: "code",
        };
        let cmd = String::from_utf8(open_command(ide, Path::new("/tmp/a b;c"))).unwrap();
        // The directory came from the filesystem and could contain anything.
        assert!(cmd.contains("'/tmp/a b;c'"), "{cmd}");
        // The command is one of our own constants; quoting it would look like a path.
        assert!(cmd.starts_with("code "), "{cmd}");
        assert!(cmd.ends_with('\r'), "a shell submits on CR");
    }

    #[test]
    fn a_path_with_a_quote_in_it_stays_one_argument() {
        let ide = Ide {
            label: "Zed",
            icon: "Ze",
            command: "zed",
        };
        let cmd = String::from_utf8(open_command(ide, Path::new("/tmp/it's"))).unwrap();
        assert_eq!(cmd, "zed '/tmp/it'\\''s'\r");
    }

    #[test]
    fn a_command_that_is_not_on_path_is_not_offered() {
        let dir = std::env::temp_dir();
        let path = std::ffi::OsString::from(dir);
        assert!(!on_path(&path, "definitely-not-a-real-editor-xyzzy"));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_without_the_execute_bit_does_not_count() {
        // A non-executable file with the right name would otherwise put a button on
        // screen that fails the moment it is pressed.
        let dir = std::env::temp_dir().join(format!("tuz-ide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("code"), b"not really").unwrap();

        let path = std::ffi::OsString::from(&dir);
        assert!(!on_path(&path, "code"), "a plain file is not a command");

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("code"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(on_path(&path, "code"), "with +x it is");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_known_editor_has_a_label_an_icon_and_a_command() {
        for ide in KNOWN {
            assert!(!ide.label.is_empty());
            assert!(!ide.command.is_empty());
            // A command with a space would need quoting we deliberately do not apply.
            assert!(!ide.command.contains(' '), "{}", ide.command);
            // The monogram is the whole button, so it has to be short enough to read
            // as a mark rather than a truncated word.
            assert!(
                (1..=2).contains(&ide.icon.chars().count()),
                "{} has a {}-character icon",
                ide.label,
                ide.icon.chars().count()
            );
        }
    }

    #[test]
    fn no_two_editors_share_a_monogram() {
        // Two identical marks side by side would be a coin flip as to which opens.
        let mut seen = std::collections::BTreeSet::new();
        for ide in KNOWN {
            assert!(seen.insert(ide.icon), "{} is used twice", ide.icon);
        }
    }

    #[test]
    fn detection_does_not_panic_without_a_path() {
        // Some launchers start a process with no PATH at all.
        assert!(!on_path(std::ffi::OsStr::new(""), "code"));
    }
}
