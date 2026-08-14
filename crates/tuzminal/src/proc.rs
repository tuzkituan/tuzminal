//! Asking the operating system what a shell is doing.
//!
//! A terminal has no way to know its shell's working directory. No escape sequence
//! reports it unless the shell opts in (OSC 7), `alacritty_terminal` 0.26 does not
//! parse that sequence anyway, and the shell's own `$PWD` lives in a process we
//! cannot read variables from. What is left is asking the kernel directly, which on
//! Linux means `/proc/<pid>/cwd`.
//!
//! That makes this module Linux-only by nature rather than by omission. Everywhere
//! else it returns `None` and the caller drops the segment — a status bar missing one
//! field is fine, a status bar that fails to draw is not.

use std::path::{Path, PathBuf};

/// The working directory of process `pid`, if it can be read.
///
/// `None` covers every ordinary failure: a process that has exited between the last
/// frame and this one, a platform without `/proc`, or a kernel that denies the read.
/// None of those are worth reporting — the directory simply is not known.
#[cfg(target_os = "linux")]
pub fn working_directory(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(target_os = "linux"))]
pub fn working_directory(_pid: u32) -> Option<PathBuf> {
    // macOS would need `proc_pidinfo` and Windows a handle we never kept; neither is
    // worth a dependency for one status segment.
    None
}

/// The user's home directory, for rendering paths relative to it.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Render `path` for display, collapsing the home directory to `~`.
///
/// Kept here rather than in the caller because it is the half of the directory
/// feature that can be tested without a running process.
pub fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        // Only a real prefix collapses. `strip_prefix` compares whole components, so
        // `/home/tuan2` is not treated as living inside `/home/tuan`.
        if path == home {
            return "~".to_owned();
        }
        if let Ok(rest) = path.strip_prefix(home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_home_directory_collapses_to_a_tilde() {
        let home = Path::new("/home/tuan");
        assert_eq!(display_path(home, Some(home)), "~");
        assert_eq!(
            display_path(Path::new("/home/tuan/hobby/tuzminal"), Some(home)),
            "~/hobby/tuzminal"
        );
    }

    #[test]
    fn a_path_outside_home_stays_absolute() {
        let home = Path::new("/home/tuan");
        assert_eq!(display_path(Path::new("/etc/hosts"), Some(home)), "/etc/hosts");
        // A shared prefix is not a parent: `/home/tuan2` must not become `~2`.
        assert_eq!(
            display_path(Path::new("/home/tuan2/src"), Some(home)),
            "/home/tuan2/src"
        );
    }

    #[test]
    fn without_a_home_every_path_stays_as_it_is() {
        assert_eq!(
            display_path(Path::new("/home/tuan/src"), None),
            "/home/tuan/src"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn our_own_working_directory_is_readable() {
        // Proves the /proc path is right, which no amount of unit testing the string
        // formatting would catch.
        let pid = std::process::id();
        let cwd = working_directory(pid).expect("a live process should report a cwd");
        assert_eq!(cwd, std::env::current_dir().unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_dead_process_reports_nothing_rather_than_failing() {
        // pid 0 is never a real process. A pane whose shell just exited takes this
        // path, and it has to be quiet about it.
        assert_eq!(working_directory(0), None);
    }
}
