//! Which shells are installed, for the new-tab menu and the settings picker.
//!
//! Read from `/etc/shells`, which is the list the system itself maintains for exactly
//! this question — `chsh` will not accept a shell that is not in it. Scanning `PATH`
//! for known names instead would find things that are not login shells and miss ones
//! installed somewhere unusual.
//!
//! Everything here degrades to an empty list rather than an error. A missing or
//! unreadable `/etc/shells` is normal on some systems, and the caller falls back to
//! `$SHELL`, which is what would have been used anyway.

use std::path::PathBuf;

/// One shell offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    /// Full path, which is what gets written to the config or handed to the PTY.
    pub path: PathBuf,
    /// Basename, which is what the menu shows.
    pub name: String,
}

/// Installed shells, in a stable order, deduplicated.
///
/// `$SHELL` is included even when `/etc/shells` does not list it: a shell someone has
/// actually set as their own is worth offering whatever the system file says.
pub fn available() -> Vec<Shell> {
    let listed = std::fs::read_to_string("/etc/shells").unwrap_or_default();
    let mut paths = parse_etc_shells(&listed);

    if let Some(current) = std::env::var_os("SHELL") {
        paths.push(PathBuf::from(current));
    }

    let mut out: Vec<Shell> = Vec::new();
    for path in paths {
        // `is_file` follows symlinks, which is what we want here: `/bin/sh` pointing
        // at dash is still a usable shell.
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        // Deduplicated by **name**, keeping the first path listed.
        //
        // Since the /usr merge, `/bin` is a symlink to `/usr/bin`, so `/etc/shells`
        // lists every shell twice — `/bin/bash` and `/usr/bin/bash` are one file — and
        // a menu built from it says "bash bash sh sh zsh zsh".
        //
        // Resolving symlinks instead would be wrong: `/bin/sh` also resolves to
        // `/usr/bin/bash`, and collapsing on that would lose `sh` entirely. It is the
        // *name* a shell is invoked under that decides its behaviour — bash started as
        // `sh` runs in POSIX mode — so the literal path is what must be launched, and
        // the name is what tells two entries apart.
        if out.iter().any(|s| s.name == name) {
            continue;
        }
        out.push(Shell { path, name });
    }

    // By name, so the menu does not reshuffle with `/etc/shells`' own ordering, which
    // is whatever order packages were installed in.
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    out
}

/// Parse the contents of `/etc/shells`.
///
/// Split out so the format handling is testable without depending on what the machine
/// running the tests happens to have installed.
fn parse_etc_shells(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(|line| line.trim())
        // `#` starts a comment, and blank lines are common between sections.
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        // Only absolute paths: a bare name would be resolved against our own working
        // directory rather than the user's, which is not what the file means.
        .filter(|line| line.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

/// The label for the "whatever the system picked" entry.
pub const DEFAULT_LABEL: &str = "Default";

/// What the settings picker offers, with the configured value first when it is not
/// one of the discovered shells.
///
/// Keeps a hand-edited `program = "/opt/weird/shell"` selectable instead of silently
/// replacing it the first time the picker is touched — the same treatment the font
/// family picker gives an unknown family.
pub fn options(configured: Option<&str>) -> Vec<String> {
    let mut out = vec![DEFAULT_LABEL.to_owned()];
    out.extend(available().into_iter().map(|s| s.path.display().to_string()));

    if let Some(configured) = configured {
        if !out.iter().any(|o| o == configured) {
            out.insert(1, configured.to_owned());
        }
    }
    out
}

/// Index of `configured` within [`options`], for the picker's initial state.
pub fn selected_index(options: &[String], configured: Option<&str>) -> usize {
    match configured {
        None => 0,
        Some(path) => options.iter().position(|o| o == path).unwrap_or(0),
    }
}

/// Turn a picker selection back into a config value.
///
/// The first entry means "no override", which is `None` — not the literal string
/// "Default", which would be run as a command.
pub fn from_selection(options: &[String], index: usize) -> Option<String> {
    if index == 0 {
        return None;
    }
    options.get(index).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = "# /etc/shells: valid login shells\n\
                    \n\
                    /bin/sh\n\
                    /bin/bash\n\
                    # a comment mid-file\n\
                    /usr/bin/zsh\n";
        assert_eq!(
            parse_etc_shells(text),
            vec![
                PathBuf::from("/bin/sh"),
                PathBuf::from("/bin/bash"),
                PathBuf::from("/usr/bin/zsh"),
            ]
        );
    }

    #[test]
    fn relative_entries_are_ignored() {
        // A bare name would resolve against our working directory, not the user's.
        assert!(parse_etc_shells("bash\n./sh\n").is_empty());
    }

    #[test]
    fn whitespace_around_a_path_does_not_break_it() {
        assert_eq!(
            parse_etc_shells("  /bin/bash  \n"),
            vec![PathBuf::from("/bin/bash")]
        );
    }

    #[test]
    fn an_empty_file_is_not_an_error() {
        assert!(parse_etc_shells("").is_empty());
        assert!(parse_etc_shells("# only comments\n").is_empty());
    }

    #[test]
    fn the_default_entry_maps_to_no_override() {
        let options = options(None);
        assert_eq!(options[0], DEFAULT_LABEL);
        assert_eq!(selected_index(&options, None), 0);
        // Not the literal word "Default", which would be run as a command.
        assert_eq!(from_selection(&options, 0), None);
    }

    #[test]
    fn a_hand_edited_shell_stays_selectable() {
        // Replacing it the moment the picker is touched would quietly undo a
        // deliberate config edit.
        let configured = "/opt/nushell/nu";
        let options = options(Some(configured));
        assert!(options.contains(&configured.to_owned()));

        let index = selected_index(&options, Some(configured));
        assert_eq!(from_selection(&options, index).as_deref(), Some(configured));
    }

    #[test]
    fn a_configured_shell_that_is_installed_is_not_listed_twice() {
        let mut options = options(None);
        // Pretend it was discovered.
        options.push("/bin/bash".to_owned());
        let with = super::options(Some("/bin/bash"));
        assert_eq!(
            with.iter().filter(|o| *o == "/bin/bash").count(),
            options.iter().filter(|o| *o == "/bin/bash").count().min(1),
            "a shell present in both places should appear once"
        );
    }

    #[test]
    fn selection_of_something_unknown_falls_back_to_default() {
        let options = options(None);
        assert_eq!(selected_index(&options, Some("/nonexistent")), 0);
    }

    #[test]
    fn discovery_returns_real_files_with_names() {
        // Whatever this machine has, every entry must be usable: an absolute path
        // that exists, and a name to show for it.
        for shell in available() {
            assert!(shell.path.is_absolute(), "{:?}", shell.path);
            assert!(shell.path.is_file(), "{:?}", shell.path);
            assert!(!shell.name.is_empty());
        }
    }

    #[test]
    fn discovery_is_sorted_and_free_of_duplicates() {
        let found = available();
        let mut sorted = found.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        assert_eq!(found, sorted, "the menu must not reshuffle between runs");

        let mut names: Vec<&String> = found.iter().map(|s| &s.name).collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(
            names.len(),
            before,
            "every /etc/shells entry is listed under both /bin and /usr/bin since the \
             /usr merge, which showed up as every shell appearing twice"
        );
    }

    #[test]
    fn sh_and_bash_stay_separate_even_though_they_are_one_binary() {
        // `/bin/sh` resolves to `/usr/bin/bash` on most Linux systems. Deduplicating
        // by resolved path would drop `sh`, and `sh` is not bash: the name it is
        // launched under puts it in POSIX mode.
        let found = available();
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        if names.contains(&"bash") && std::path::Path::new("/bin/sh").is_file() {
            assert!(
                names.contains(&"sh"),
                "sh should survive alongside bash, got {names:?}"
            );
        }
    }

    #[test]
    fn a_shells_name_is_its_basename() {
        // What the menu shows for each row.
        for shell in available() {
            let expected = shell.path.file_name().unwrap().to_string_lossy();
            assert_eq!(shell.name, expected);
        }
    }
}
