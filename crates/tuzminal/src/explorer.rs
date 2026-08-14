//! The file explorer sidebar.
//!
//! Follows the shape of [`crate::settings::SettingsPanel`]: state plus a
//! [`tuz_ui::Ui`], rebuilt into widgets every frame and driven by the app. The
//! listing, sorting, formatting and quoting are all pure functions over plain data,
//! so the parts most likely to be wrong are the parts testable without a GPU, a
//! terminal or a window.
//!
//! # Writing paths into a live shell
//!
//! Three of the four actions hand a path to a running shell. A file name may contain
//! a quote, a semicolon or `$(...)` — all legal, and all arriving routinely via a
//! `git clone` or an unpacked archive. Passing one unquoted is command injection with
//! the filesystem as the attack surface, so every path goes through [`shell_quote`].

use std::path::{Path, PathBuf};
use tuz_ui::{EntryKind, Ui, Widget, WidgetId};

/// Largest directory we will list.
///
/// Not a memory limit — it is a latency one. Every row is cloned into the widget list
/// each frame, and a directory with a hundred thousand entries would stall the frame
/// that opened it. The overflow is reported rather than silently dropped.
const MAX_ENTRIES: usize = 5000;

/// Ids are positional here, unlike the settings panel where they name a setting.
///
/// A file list has no stable identity to key on — the rows change wholesale on every
/// navigation — so focus is meant to reset when the directory does.
const ROW_ID_BASE: u32 = 1000;
const PROMPT_ID: WidgetId = WidgetId(1);

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// Size in bytes for a file; entry count for a directory. `None` when unreadable.
    pub size: Option<u64>,
}

impl Entry {
    /// The dimmed right-hand column.
    fn detail(&self) -> String {
        match self.kind {
            EntryKind::Parent => String::new(),
            EntryKind::Directory => match self.size {
                Some(n) => format!("{n}"),
                None => String::new(),
            },
            _ => match self.size {
                Some(n) => human_size(n),
                None => String::new(),
            },
        }
    }
}

/// A modal question at the foot of the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    Rename {
        from: PathBuf,
        value: String,
    },
    NewFolder {
        value: String,
    },
    /// `entries` is how many things are inside, so the confirmation can say what it
    /// is about to destroy rather than just asking.
    Delete {
        path: PathBuf,
        entries: usize,
    },
}

/// What the app should do after handling an explorer event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExplorerOutcome {
    /// Nothing happened worth a frame.
    Continue,
    Redraw,
    /// Type this path at the shell prompt, without running it.
    InsertPath(PathBuf),
    /// Run `cd` in the focused shell.
    RunCd(PathBuf),
    /// Run `$EDITOR` against this file in the focused shell.
    OpenEditor(PathBuf),
    /// Give the keyboard back to the terminal, staying open.
    Unfocus,
}

pub struct Explorer {
    pub ui: Ui,
    dir: PathBuf,
    entries: Vec<Entry>,
    /// Entries beyond [`MAX_ENTRIES`], reported rather than hidden.
    truncated: usize,
    selected: usize,
    show_hidden: bool,
    prompt: Option<Prompt>,
}

impl Explorer {
    pub fn open(dir: PathBuf, show_hidden: bool) -> Self {
        let mut explorer = Self {
            ui: Ui::new(),
            dir,
            entries: Vec::new(),
            truncated: 0,
            selected: 0,
            show_hidden,
            prompt: None,
        };
        explorer.refresh();
        explorer
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn prompt(&self) -> Option<&Prompt> {
        self.prompt.as_ref()
    }
    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// Re-read the current directory.
    pub fn refresh(&mut self) {
        let (entries, truncated) = read_dir(&self.dir, self.show_hidden);
        self.entries = entries;
        self.truncated = truncated;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        if self.show_hidden != show {
            self.show_hidden = show;
            self.refresh();
        }
    }

    /// Move into `dir`, resetting the selection.
    fn navigate(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.refresh();
        // Back to the top, and the scroll with it: arriving in a new directory
        // already scrolled halfway down would be disorienting.
        self.ui.scroll_by(i32::MIN / 2);
        self.select(0);
    }

    /// The rows, rebuilt each frame.
    pub fn widgets(&self) -> Vec<Widget> {
        let mut out: Vec<Widget> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                Widget::entry(
                    WidgetId(ROW_ID_BASE + i as u32),
                    entry.kind,
                    entry.name.clone(),
                    entry.detail(),
                    i == self.selected,
                )
            })
            .collect();

        if self.truncated > 0 {
            // A silent cap would read as "this directory has 5000 files".
            out.push(Widget::label(format!("… {} more", self.truncated)));
        }
        out
    }

    /// The prompt row, if one is open. Empty otherwise, so the footer collapses.
    pub fn footer_widgets(&self) -> Vec<Widget> {
        match &self.prompt {
            None => Vec::new(),
            Some(Prompt::Rename { value, .. }) => {
                vec![Widget::text(PROMPT_ID, "Rename to", value.clone(), "")]
            }
            Some(Prompt::NewFolder { value }) => {
                vec![Widget::text(PROMPT_ID, "New folder", value.clone(), "")]
            }
            Some(Prompt::Delete { path, entries }) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                // The count is the whole point: "Delete build?" and "Delete build, and
                // the 12043 things inside it?" are different questions.
                let text = if *entries > 0 {
                    format!("Delete {name} and {entries} inside? (y/n)")
                } else {
                    format!("Delete {name}? (y/n)")
                };
                vec![Widget::label(text)]
            }
        }
    }

    /// Put the keyboard focus on an open prompt, so its caret is drawn.
    ///
    /// Called after layout rather than when the prompt opens: `Ui::focus` only accepts
    /// an id it has actually placed, and the field does not exist until the frame
    /// after it is asked for.
    pub fn focus_prompt(&mut self) {
        if self.prompt.is_some() {
            self.ui.focus(PROMPT_ID);
        }
    }

    /// Move the selection by `delta` rows, clamped.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let last = self.entries.len() as i32 - 1;
        let next = (self.selected as i32 + delta).clamp(0, last) as usize;
        let moved = next != self.selected;
        self.select(next);
        moved
    }

    /// Move the selection to `index`, keeping the `Ui`'s focus with it.
    ///
    /// The two have to agree: scrolling follows the `Ui`'s focus, so a selection that
    /// moved without it would walk off the bottom of the viewport and stay there.
    fn select(&mut self, index: usize) {
        self.selected = index;
        self.ui.focus(WidgetId(ROW_ID_BASE + index as u32));
        self.ui.scroll_to_focus_in_body();
    }

    /// Activate the selected row: descend into a directory, or open a file.
    pub fn activate(&mut self) -> ExplorerOutcome {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return ExplorerOutcome::Continue;
        };
        match entry.kind {
            EntryKind::Parent | EntryKind::Directory => {
                self.navigate(entry.path);
                ExplorerOutcome::Redraw
            }
            // Enter on a file opens it, the way it does in every file browser.
            // Typing its path instead is `p`, which is the rarer thing to want.
            _ => ExplorerOutcome::OpenEditor(entry.path),
        }
    }

    /// Handle a click on the row with `id`.
    ///
    /// Selects it, or activates it when it was already the selection. Two clicks
    /// rather than one because a single click that navigated would make the list
    /// unbrowsable with a mouse: every attempt to look at a row would move you into it.
    pub fn click_row(&mut self, id: WidgetId) -> ExplorerOutcome {
        let Some(index) = id.0.checked_sub(ROW_ID_BASE).map(|i| i as usize) else {
            return ExplorerOutcome::Continue;
        };
        if index >= self.entries.len() {
            return ExplorerOutcome::Continue;
        }
        if self.selected == index {
            return self.activate();
        }
        self.select(index);
        ExplorerOutcome::Redraw
    }

    /// Go to the parent directory.
    pub fn go_up(&mut self) -> bool {
        let Some(parent) = self.dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        self.navigate(parent);
        true
    }

    // --- prompts ---------------------------------------------------------

    pub fn begin_rename(&mut self) -> bool {
        let Some(entry) = self.entries.get(self.selected) else {
            return false;
        };
        if entry.kind == EntryKind::Parent {
            return false;
        }
        self.prompt = Some(Prompt::Rename {
            from: entry.path.clone(),
            value: entry.name.clone(),
        });
        true
    }

    pub fn begin_new_folder(&mut self) {
        self.prompt = Some(Prompt::NewFolder {
            value: String::new(),
        });
    }

    pub fn begin_delete(&mut self) -> bool {
        let Some(entry) = self.entries.get(self.selected) else {
            return false;
        };
        if entry.kind == EntryKind::Parent {
            return false;
        }
        // Counted now, while the prompt is being built, so the question can state it.
        let entries = if entry.kind == EntryKind::Directory {
            std::fs::read_dir(&entry.path)
                .map(|d| d.flatten().count())
                .unwrap_or(0)
        } else {
            0
        };
        self.prompt = Some(Prompt::Delete {
            path: entry.path.clone(),
            entries,
        });
        true
    }

    pub fn cancel_prompt(&mut self) -> bool {
        self.prompt.take().is_some()
    }

    /// Type into an open text prompt.
    pub fn prompt_input(&mut self, text: &str) -> bool {
        match &mut self.prompt {
            Some(Prompt::Rename { value, .. }) | Some(Prompt::NewFolder { value }) => {
                value.push_str(text);
                true
            }
            _ => false,
        }
    }

    /// Remove the last character of an open text prompt.
    pub fn prompt_backspace(&mut self) -> bool {
        match &mut self.prompt {
            Some(Prompt::Rename { value, .. }) | Some(Prompt::NewFolder { value }) => {
                value.pop().is_some()
            }
            _ => false,
        }
    }

    /// Carry out the open prompt. `Err` carries a message for a toast.
    ///
    /// Returns `Ok(false)` when there was nothing to do, so a stray Enter is not
    /// reported as a success.
    pub fn commit_prompt(&mut self) -> Result<bool, String> {
        let Some(prompt) = self.prompt.take() else {
            return Ok(false);
        };

        let result = match &prompt {
            Prompt::Rename { from, value } => {
                let name = value.trim();
                if name.is_empty() {
                    // Put the prompt back rather than silently doing nothing.
                    self.prompt = Some(prompt.clone());
                    return Err("a name cannot be empty".to_owned());
                }
                // `is_separator`, not `MAIN_SEPARATOR`: on Windows both `/` and `\`
                // separate paths, and `fs::rename` honours both — so checking only the
                // primary one let `../escaped.txt` through and turned a rename into a
                // move. On Unix `\` is a legal filename character and stays allowed.
                if name.chars().any(std::path::is_separator) {
                    self.prompt = Some(prompt.clone());
                    return Err("a name cannot contain a path separator".to_owned());
                }
                let to = self.dir.join(name);
                // `rename` overwrites an existing file silently on Unix. Renaming onto
                // a neighbour would destroy it with no prompt at all, which is worse
                // than the delete this feature does ask before doing.
                if to.symlink_metadata().is_ok() && to != *from {
                    self.prompt = Some(prompt.clone());
                    return Err(format!("{name} already exists"));
                }
                std::fs::rename(from, &to).map_err(|e| format!("could not rename: {e}"))
            }
            Prompt::NewFolder { value } => {
                let name = value.trim();
                if name.is_empty() {
                    self.prompt = Some(prompt.clone());
                    return Err("a name cannot be empty".to_owned());
                }
                std::fs::create_dir(self.dir.join(name))
                    .map_err(|e| format!("could not create the folder: {e}"))
            }
            Prompt::Delete { path, .. } => {
                delete(path).map_err(|e| format!("could not delete: {e}"))
            }
        };

        self.refresh();
        result.map(|_| true)
    }
}

/// Remove a path, following the rule that decides whether this is safe.
///
/// `Path::is_dir` and `fs::metadata` both **follow symlinks**, so a link pointing at a
/// directory reports `is_dir() == true`. Deleting it through that answer calls
/// `remove_dir_all` on the link and walks into the target, destroying a tree the user
/// never selected and which may not even be nearby. `symlink_metadata` does not
/// follow, so a link is recognised as a link and `remove_file` unlinks it alone.
fn delete(path: &Path) -> std::io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        // Covers files and symlinks of every kind, including broken ones.
        std::fs::remove_file(path)
    }
}

/// Read and sort a directory.
///
/// Follows the posture already established by `Theme::available` and
/// `plugin::discover`: an unreadable directory or entry is skipped rather than
/// raised, because a browser that refuses to show anything because one file could not
/// be stat'd is worse than one showing the rest.
///
/// The sort is explicit and load-bearing — `read_dir` order is not deterministic, so
/// without it the list would reshuffle between visits to the same directory.
fn read_dir(dir: &Path, show_hidden: bool) -> (Vec<Entry>, usize) {
    let mut entries = Vec::new();

    if dir.parent().is_some() {
        entries.push(Entry {
            name: "..".to_owned(),
            path: dir.parent().unwrap().to_path_buf(),
            kind: EntryKind::Parent,
            size: None,
        });
    }

    let Ok(read) = std::fs::read_dir(dir) else {
        return (entries, 0);
    };

    let mut rows: Vec<Entry> = Vec::new();
    let mut truncated = 0usize;

    for item in read.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if rows.len() >= MAX_ENTRIES {
            truncated += 1;
            continue;
        }

        let path = item.path();
        // `file_type` comes from `lstat` and does not follow links, which is what
        // makes a symlink list as a symlink instead of as whatever it points at. That
        // distinction is not cosmetic — see `delete_kind`.
        let file_type = item.file_type().ok();
        let link = file_type.map(|t| t.is_symlink()).unwrap_or(false);
        let is_dir = !link && file_type.map(|t| t.is_dir()).unwrap_or(false);

        let kind = if link {
            EntryKind::Symlink
        } else if is_dir {
            EntryKind::Directory
        } else {
            EntryKind::File
        };

        let size = if is_dir {
            std::fs::read_dir(&path)
                .ok()
                .map(|d| d.flatten().count() as u64)
        } else {
            // A broken link has no metadata; the row still lists, without a size.
            std::fs::symlink_metadata(&path).ok().map(|m| m.len())
        };

        rows.push(Entry {
            name,
            path,
            kind,
            size,
        });
    }

    sort_entries(&mut rows);
    entries.extend(rows);
    (entries, truncated)
}

/// Directories above files, then by name, case-insensitively.
///
/// Case-insensitive because `Makefile` sorting far from `main.rs` is the kind of
/// ordering only a byte comparison would choose. Ties break on the raw name so the
/// order is total and therefore stable.
fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let rank = |e: &Entry| match e.kind {
            EntryKind::Parent => 0,
            EntryKind::Directory => 1,
            _ => 2,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// A byte count at a glance.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < UNITS.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else if size < 10.0 {
        format!("{size:.1}{}", UNITS[unit])
    } else {
        format!("{size:.0}{}", UNITS[unit])
    }
}

/// Quote a string as a single POSIX shell argument.
///
/// Single quotes are the only form under which every byte is literal — no expansion,
/// no substitution, no escape processing — with the sole exception of `'` itself,
/// which cannot appear inside them at all. So the quoted run is closed, a literal
/// quote is emitted escaped, and a new run is opened: `it's` becomes `'it'\''s'`.
///
/// This is the boundary between a filename and a command. Without it, a file called
/// `$(rm -rf ~)` in a directory you merely *browsed* would run when you pressed the
/// key to type its path.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A `cd` command for `dir`, ready to write to a PTY.
///
/// Terminated with `\r`, not `\n`: carriage return is what the Enter key sends and
/// what a shell submits on.
pub fn cd_command(dir: &Path) -> Vec<u8> {
    format!("cd {}\r", shell_quote(&dir.to_string_lossy())).into_bytes()
}

/// An `$EDITOR` invocation for `path`.
///
/// `$EDITOR` is expanded by the shell rather than by us, so an unset variable produces
/// the shell's own diagnostic instead of us guessing at `vi`.
pub fn editor_command(path: &Path) -> Vec<u8> {
    format!(
        "\"${{EDITOR:-vi}}\" {}\r",
        shell_quote(&path.to_string_lossy())
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- the one that matters most ---------------------------------------

    #[test]
    fn shell_quoting_neutralizes_every_metacharacter() {
        // Each of these is a legal filename and each is a command if left unquoted.
        for name in [
            "it's",
            "a;rm -rf b",
            "$(whoami)",
            "`whoami`",
            "a b",
            "a\nb",
            "&& echo pwned",
            "|tee /tmp/x",
            "~/secret",
            "*",
            "",
        ] {
            let quoted = shell_quote(name);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{name:?} produced {quoted:?}, which is not a quoted run"
            );
            // Every `'` inside must be the escaped form, never a bare one that would
            // close the run early and expose the rest to the shell.
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !inner.contains('\'') || inner.contains(r"'\''"),
                "{name:?} produced {quoted:?} with an unescaped quote"
            );
        }
    }

    #[test]
    fn a_quote_in_a_name_is_closed_escaped_and_reopened() {
        // The exact expansion, because this is the case a naive implementation gets
        // wrong and the one that breaks out of the quoting.
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("'"), r"''\'''");
    }

    #[test]
    fn commands_end_in_a_carriage_return_not_a_newline() {
        let cmd = cd_command(Path::new("/tmp"));
        assert_eq!(cmd.last(), Some(&b'\r'), "a shell submits on CR, not LF");
        assert!(!cmd.contains(&b'\n'));

        let cmd = String::from_utf8(cd_command(Path::new("/tmp/it's"))).unwrap();
        assert_eq!(cmd, "cd '/tmp/it'\\''s'\r");
    }

    #[test]
    fn the_editor_command_quotes_its_argument() {
        let cmd = String::from_utf8(editor_command(Path::new("/tmp/a b;c"))).unwrap();
        assert!(cmd.contains("'/tmp/a b;c'"), "{cmd}");
        assert!(cmd.ends_with('\r'));
    }

    // --- sorting ---------------------------------------------------------

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_owned(),
            path: PathBuf::from(name),
            kind,
            size: None,
        }
    }

    #[test]
    fn directories_sort_above_files_and_the_parent_above_both() {
        let mut rows = vec![
            entry("zebra.txt", EntryKind::File),
            entry("src", EntryKind::Directory),
            entry("..", EntryKind::Parent),
            entry("Cargo.toml", EntryKind::File),
            entry("assets", EntryKind::Directory),
        ];
        sort_entries(&mut rows);
        let names: Vec<&str> = rows.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "assets", "src", "Cargo.toml", "zebra.txt"]);
    }

    #[test]
    fn sorting_ignores_case() {
        let mut rows = vec![
            entry("banana", EntryKind::File),
            entry("Apple", EntryKind::File),
            entry("Cherry", EntryKind::File),
        ];
        sort_entries(&mut rows);
        let names: Vec<&str> = rows.iter().map(|e| e.name.as_str()).collect();
        // A byte sort puts every capitalised name first, so `Apple` and `Cherry` would
        // bracket `banana` instead of reading alphabetically.
        assert_eq!(names, ["Apple", "banana", "Cherry"]);
    }

    #[test]
    fn names_differing_only_in_case_have_a_stable_total_order() {
        // Case-insensitive comparison alone leaves these equal, so the sort could
        // return either order and the list would reshuffle between visits. The raw
        // name is the tiebreak that makes the ordering total.
        let mut a = vec![
            entry("Makefile", EntryKind::File),
            entry("MAKEFILE", EntryKind::File),
        ];
        let mut b = vec![
            entry("MAKEFILE", EntryKind::File),
            entry("Makefile", EntryKind::File),
        ];
        sort_entries(&mut a);
        sort_entries(&mut b);
        assert_eq!(
            a, b,
            "the same set must sort the same way whatever order it arrives in"
        );
    }

    #[test]
    fn sizes_read_as_sizes() {
        assert_eq!(human_size(0), "0B");
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(20 * 1024), "20K");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0M");
    }

    // --- listing, against a real directory --------------------------------

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("tuz-explorer-{tag}-{}", std::process::id()));
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
    fn hidden_files_appear_only_when_asked_for() {
        let tmp = TempDir::new("hidden");
        std::fs::write(tmp.0.join("visible.txt"), b"x").unwrap();
        std::fs::write(tmp.0.join(".hidden"), b"x").unwrap();

        let (shown, _) = read_dir(&tmp.0, false);
        assert!(shown.iter().all(|e| e.name != ".hidden"));
        assert!(shown.iter().any(|e| e.name == "visible.txt"));

        let (all, _) = read_dir(&tmp.0, true);
        assert!(all.iter().any(|e| e.name == ".hidden"));
    }

    #[test]
    fn an_unreadable_directory_lists_as_empty_rather_than_failing() {
        // A browser that errors out on a directory it cannot read is a browser that
        // cannot recover; it should show the parent row and nothing else.
        let (entries, truncated) = read_dir(Path::new("/nonexistent-tuzminal-path"), false);
        assert_eq!(truncated, 0);
        assert!(entries.iter().all(|e| e.kind == EntryKind::Parent));
    }

    #[test]
    fn the_root_directory_has_no_parent_row() {
        let (entries, _) = read_dir(Path::new("/"), false);
        assert!(
            entries.iter().all(|e| e.kind != EntryKind::Parent),
            "`/` has no parent, so offering one would navigate nowhere"
        );
    }

    #[test]
    fn create_rename_and_delete_round_trip() {
        let tmp = TempDir::new("ops");
        let mut ex = Explorer::open(tmp.0.clone(), false);

        ex.begin_new_folder();
        ex.prompt_input("work");
        assert_eq!(ex.commit_prompt(), Ok(true));
        assert!(tmp.0.join("work").is_dir());

        // Select it, then rename.
        ex.selected = ex.entries.iter().position(|e| e.name == "work").unwrap();
        assert!(ex.begin_rename());
        // Replace the prefilled name.
        for _ in 0.."work".len() {
            ex.prompt_backspace();
        }
        ex.prompt_input("done");
        assert_eq!(ex.commit_prompt(), Ok(true));
        assert!(tmp.0.join("done").is_dir());
        assert!(!tmp.0.join("work").exists());

        // Delete, with something inside so the recursive path is the one exercised.
        std::fs::write(tmp.0.join("done/file.txt"), b"x").unwrap();
        ex.refresh();
        ex.selected = ex.entries.iter().position(|e| e.name == "done").unwrap();
        assert!(ex.begin_delete());
        match ex.prompt() {
            Some(Prompt::Delete { entries, .. }) => {
                assert_eq!(
                    *entries, 1,
                    "the prompt must say what it is about to destroy"
                )
            }
            other => panic!("expected a delete prompt, got {other:?}"),
        }
        assert_eq!(ex.commit_prompt(), Ok(true));
        assert!(!tmp.0.join("done").exists());
    }

    #[cfg(unix)]
    #[test]
    fn deleting_a_symlink_to_a_directory_does_not_touch_the_target() {
        // The most dangerous bug this feature could have. `Path::is_dir` and
        // `fs::metadata` both follow symlinks, so a link pointing at a directory
        // answers `is_dir() == true`; deleting through that answer calls
        // `remove_dir_all` on the link and walks into the target, destroying a tree
        // the user never selected.
        let tmp = TempDir::new("symlink");
        std::fs::create_dir(tmp.0.join("real")).unwrap();
        std::fs::write(tmp.0.join("real/keep.txt"), b"precious").unwrap();
        std::os::unix::fs::symlink(tmp.0.join("real"), tmp.0.join("link")).unwrap();

        delete(&tmp.0.join("link")).expect("the link should be removable");

        assert!(!tmp.0.join("link").exists(), "the link itself is gone");
        assert!(
            tmp.0.join("real/keep.txt").exists(),
            "the target tree must survive: deleting a link is not deleting what it points at"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_lists_as_a_link_not_as_its_target() {
        let tmp = TempDir::new("symkind");
        std::fs::create_dir(tmp.0.join("real")).unwrap();
        std::os::unix::fs::symlink(tmp.0.join("real"), tmp.0.join("link")).unwrap();

        let (entries, _) = read_dir(&tmp.0, false);
        let link = entries.iter().find(|e| e.name == "link").unwrap();
        // If this reports Directory, `delete` would have taken the recursive path.
        assert_eq!(link.kind, EntryKind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_lists_rather_than_disappearing() {
        let tmp = TempDir::new("broken");
        std::os::unix::fs::symlink(tmp.0.join("nowhere"), tmp.0.join("dangling")).unwrap();

        let (entries, _) = read_dir(&tmp.0, false);
        assert!(
            entries.iter().any(|e| e.name == "dangling"),
            "a link with no target is still a thing you can see and delete"
        );
    }

    #[test]
    fn renaming_onto_an_existing_name_is_refused() {
        // `fs::rename` overwrites silently on Unix. Losing a neighbouring file to a
        // rename would be worse than the delete this feature does ask about.
        let tmp = TempDir::new("clobber");
        std::fs::write(tmp.0.join("keep.txt"), b"precious").unwrap();
        std::fs::write(tmp.0.join("other.txt"), b"x").unwrap();

        let mut ex = Explorer::open(tmp.0.clone(), false);
        ex.selected = ex
            .entries
            .iter()
            .position(|e| e.name == "other.txt")
            .unwrap();
        assert!(ex.begin_rename());
        for _ in 0.."other.txt".len() {
            ex.prompt_backspace();
        }
        ex.prompt_input("keep.txt");

        assert!(ex.commit_prompt().is_err());
        assert_eq!(
            std::fs::read(tmp.0.join("keep.txt")).unwrap(),
            b"precious",
            "the existing file must be untouched"
        );
        assert!(tmp.0.join("other.txt").exists());
        assert!(
            ex.prompt().is_some(),
            "and the prompt stays open to be corrected"
        );
    }

    #[test]
    fn a_rename_cannot_become_a_move() {
        let tmp = TempDir::new("escape");
        std::fs::write(tmp.0.join("a.txt"), b"x").unwrap();
        let mut ex = Explorer::open(tmp.0.clone(), false);
        ex.selected = ex.entries.iter().position(|e| e.name == "a.txt").unwrap();
        assert!(ex.begin_rename());
        for _ in 0.."a.txt".len() {
            ex.prompt_backspace();
        }
        // Rename means rename. A separator here would relocate the file anywhere on
        // the filesystem from a field labelled "Rename to".
        ex.prompt_input("../escaped.txt");
        assert!(ex.commit_prompt().is_err());
        assert!(tmp.0.join("a.txt").exists());
    }

    #[test]
    fn an_empty_name_is_refused_and_keeps_the_prompt_open() {
        let tmp = TempDir::new("empty");
        let mut ex = Explorer::open(tmp.0.clone(), false);

        ex.begin_new_folder();
        ex.prompt_input("   ");
        assert!(ex.commit_prompt().is_err());
        // Still open, so the mistake is visible and correctable rather than swallowed.
        assert!(ex.prompt().is_some());
    }

    #[test]
    fn escape_cancels_a_prompt_without_touching_the_filesystem() {
        let tmp = TempDir::new("cancel");
        let mut ex = Explorer::open(tmp.0.clone(), false);

        ex.begin_new_folder();
        ex.prompt_input("nope");
        assert!(ex.cancel_prompt());
        assert!(ex.prompt().is_none());
        assert!(!tmp.0.join("nope").exists());
        assert!(!ex.cancel_prompt(), "cancelling twice is not a cancel");
    }

    #[test]
    fn navigating_into_a_directory_and_back_out_lands_where_it_started() {
        let tmp = TempDir::new("nav");
        std::fs::create_dir(tmp.0.join("child")).unwrap();
        let mut ex = Explorer::open(tmp.0.clone(), false);

        ex.selected = ex.entries.iter().position(|e| e.name == "child").unwrap();
        assert_eq!(ex.activate(), ExplorerOutcome::Redraw);
        assert_eq!(ex.dir(), tmp.0.join("child"));

        assert!(ex.go_up());
        assert_eq!(ex.dir(), tmp.0);
    }

    #[test]
    fn activating_a_file_opens_it_rather_than_navigating() {
        let tmp = TempDir::new("file");
        std::fs::write(tmp.0.join("notes.txt"), b"x").unwrap();
        let mut ex = Explorer::open(tmp.0.clone(), false);

        ex.selected = ex
            .entries
            .iter()
            .position(|e| e.name == "notes.txt")
            .unwrap();
        assert_eq!(
            ex.activate(),
            ExplorerOutcome::OpenEditor(tmp.0.join("notes.txt"))
        );
        assert_eq!(
            ex.dir(),
            tmp.0,
            "activating a file must not change directory"
        );
    }

    #[test]
    fn moving_the_selection_marks_exactly_one_row_selected() {
        // The bug this guards: the selection moved but nothing drew it, so the arrow
        // keys looked broken. The widget list is what drawing reads, so assert there.
        let tmp = TempDir::new("selected");
        for name in ["a", "b", "c"] {
            std::fs::write(tmp.0.join(name), b"x").unwrap();
        }
        let mut ex = Explorer::open(tmp.0.clone(), false);

        let marked = |ex: &Explorer| -> Vec<usize> {
            ex.widgets()
                .iter()
                .enumerate()
                .filter_map(|(i, w)| match w {
                    Widget::Entry { selected: true, .. } => Some(i),
                    _ => None,
                })
                .collect()
        };

        assert_eq!(marked(&ex), vec![0]);
        ex.move_selection(1);
        assert_eq!(marked(&ex), vec![1], "exactly one row, and it moved");
        ex.move_selection(1);
        assert_eq!(marked(&ex), vec![2]);
    }

    #[test]
    fn the_selection_clamps_instead_of_running_off_either_end() {
        let tmp = TempDir::new("clamp");
        std::fs::write(tmp.0.join("a"), b"x").unwrap();
        let mut ex = Explorer::open(tmp.0.clone(), false);

        assert!(!ex.move_selection(-1), "already at the top");
        ex.move_selection(1000);
        assert_eq!(ex.selected, ex.entries.len() - 1);
        assert!(!ex.move_selection(1), "already at the bottom");
    }

    #[test]
    fn the_parent_row_cannot_be_renamed_or_deleted() {
        let tmp = TempDir::new("parent");
        let mut ex = Explorer::open(tmp.0.clone(), false);
        ex.selected = ex
            .entries
            .iter()
            .position(|e| e.kind == EntryKind::Parent)
            .expect("a temp dir has a parent");

        // `..` is the directory above, not an entry in this one; renaming or deleting
        // through it would act on something the user is not looking at.
        assert!(!ex.begin_rename());
        assert!(!ex.begin_delete());
        assert!(ex.prompt().is_none());
    }
}
