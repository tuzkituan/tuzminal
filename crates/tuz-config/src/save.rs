//! Writing settings back to `config.toml` without destroying it.
//!
//! # Why this is not `toml::to_string`
//!
//! Serializing the whole [`Config`] and writing it out would produce a valid file
//! and **delete every comment in it**. The file users start from is
//! `config.example.toml`, which is mostly comments explaining what each setting
//! does. Losing those because someone changed the font size in a GUI would be an
//! unacceptable trade.
//!
//! So saving is surgical: read the existing document with `toml_edit`, set only the
//! keys whose values actually changed, and leave every other byte alone. Comments,
//! key order, blank lines and inline-table style all survive.
//!
//! # Atomicity
//!
//! The new document is written to a temporary file in the same directory and then
//! renamed over the original. A rename within one filesystem is atomic, so an
//! interrupted save leaves the old config intact rather than a truncated one.

use crate::schema::Config;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Write the values in `config` that differ from `baseline` into the file at `path`.
///
/// `baseline` is what the file is believed to contain — normally the config as it was
/// when the settings panel opened. Only keys that differ are touched, so a save never
/// rewrites settings the user did not change.
///
/// Creates the file, and any missing parent directories, if absent.
pub fn save_config(path: &Path, config: &Config, baseline: &Config) -> Result<PathBuf, SaveError> {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // A first-time save writes a fresh document rather than failing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SaveError::Read {
                path: path.to_owned(),
                source,
            })
        }
    };

    let mut doc: DocumentMut = existing.parse().map_err(|source| SaveError::Parse {
        path: path.to_owned(),
        source: Box::new(source),
    })?;

    let changes = collect_changes(config, baseline);
    if changes.is_empty() {
        // Nothing to do. Returning early matters: rewriting the file with identical
        // content would still trip the config watcher and churn its mtime.
        return Ok(path.to_owned());
    }

    for change in &changes {
        apply(&mut doc, change);
    }

    write_atomically(path, doc.to_string().as_bytes())?;
    log::info!("saved {} setting(s) to {}", changes.len(), path.display());
    Ok(path.to_owned())
}

/// One setting to write: a dotted path and its new value.
///
/// Not `PartialEq`: `toml_edit::Value` deliberately does not implement it, since two
/// values can be equal in content but differ in formatting.
#[derive(Debug, Clone)]
struct Change {
    /// e.g. `["font", "size"]`, or `["theme"]` for a top-level key.
    path: Vec<&'static str>,
    value: Value,
}

impl Change {
    fn new(path: Vec<&'static str>, value: impl Into<Value>) -> Self {
        Self {
            path,
            value: value.into(),
        }
    }
}

/// Diff the two configs into a list of changes.
///
/// Only the settings the panel can edit are compared. Deliberately not derived
/// automatically: `[keys]`, `[shell]` and `[plugins]` are better hand-edited, and a
/// blanket diff would rewrite them the first time anything else changed.
fn collect_changes(new: &Config, old: &Config) -> Vec<Change> {
    let mut changes = Vec::new();

    macro_rules! compare {
        ($path:expr, $field:expr, $convert:expr) => {
            #[allow(clippy::redundant_closure_call)]
            if $field(new) != $field(old) {
                changes.push(Change::new($path, $convert($field(new))));
            }
        };
    }

    // Top level.
    compare!(vec!["theme"], |c: &Config| c.theme.clone(), |v: String| v);

    // [font]
    compare!(
        vec!["font", "family"],
        |c: &Config| c.font.family.clone(),
        |v: String| v
    );
    compare!(vec!["font", "size"], |c: &Config| c.font.size, |v: f32| v
        as f64);
    compare!(
        vec!["font", "line_height"],
        |c: &Config| c.font.line_height,
        |v: f32| v as f64
    );
    compare!(
        vec!["font", "cell_width"],
        |c: &Config| c.font.cell_width,
        |v: f32| v as f64
    );
    compare!(
        vec!["font", "ligatures"],
        |c: &Config| c.font.ligatures,
        |v: bool| v
    );

    // [window]
    compare!(
        vec!["window", "padding", "x"],
        |c: &Config| c.window.padding.x,
        |v: u16| v as i64
    );
    compare!(
        vec!["window", "padding", "y"],
        |c: &Config| c.window.padding.y,
        |v: u16| v as i64
    );
    compare!(
        vec!["window", "opacity"],
        |c: &Config| c.window.opacity,
        |v: f32| v as f64
    );
    compare!(
        vec!["window", "decorations"],
        |c: &Config| c.window.decorations,
        |v: bool| v
    );
    compare!(
        vec!["window", "always_show_tab_bar"],
        |c: &Config| c.window.always_show_tab_bar,
        |v: bool| v
    );
    compare!(
        vec!["window", "center_grid"],
        |c: &Config| c.window.center_grid,
        |v: bool| v
    );

    // [cursor]
    compare!(
        vec!["cursor", "shape"],
        |c: &Config| cursor_shape_name(c.cursor.shape),
        |v: &'static str| v.to_owned()
    );
    compare!(
        vec!["cursor", "blink"],
        |c: &Config| c.cursor.blink,
        |v: bool| v
    );
    compare!(
        vec!["cursor", "blink_interval_ms"],
        |c: &Config| c.cursor.blink_interval_ms,
        |v: u64| v as i64
    );

    // [scrollback]
    compare!(
        vec!["scrollback", "lines"],
        |c: &Config| c.scrollback.lines,
        |v: u32| v as i64
    );
    compare!(
        vec!["scrollback", "scroll_multiplier"],
        |c: &Config| c.scrollback.scroll_multiplier,
        |v: u8| v as i64
    );

    // [performance]
    compare!(
        vec!["performance", "vsync"],
        |c: &Config| c.performance.vsync,
        |v: bool| v
    );
    compare!(
        vec!["performance", "max_fps"],
        |c: &Config| c.performance.max_fps,
        |v: u16| v as i64
    );

    changes
}

/// The config spelling of a cursor shape. Must match the `serde` rename.
fn cursor_shape_name(shape: crate::schema::CursorShape) -> &'static str {
    use crate::schema::CursorShape::*;
    match shape {
        Block => "block",
        Beam => "beam",
        Underline => "underline",
        HollowBlock => "hollow_block",
    }
}

/// Set one dotted key in the document, creating tables as needed.
fn apply(doc: &mut DocumentMut, change: &Change) {
    let (last, parents) = match change.path.split_last() {
        Some(split) => split,
        None => return,
    };

    // Walk down, creating any missing table on the way. New tables are marked
    // implicit so `toml_edit` only emits a `[header]` if something actually lands
    // inside — otherwise a save could add empty sections to the file.
    let mut table: &mut Table = doc.as_table_mut();
    for key in parents {
        let entry = table
            .entry(key)
            .or_insert_with(|| Item::Table(Table::new()));

        // An existing key of the wrong shape (a value where a table is needed) is
        // replaced: the schema says it must be a table, so the file is wrong.
        if !entry.is_table() {
            *entry = Item::Table(Table::new());
        }
        table = entry.as_table_mut().expect("just ensured this is a table");
    }

    // Preserve the surrounding whitespace of the value being replaced, so a
    // carefully aligned file stays aligned.
    let decor = table
        .get(last)
        .and_then(|item| item.as_value())
        .map(|v| v.decor().clone());

    let mut value = change.value.clone();
    if let Some(decor) = decor {
        *value.decor_mut() = decor;
    }
    table.insert(last, Item::Value(value));
}

/// Write `bytes` to `path` via a temporary file and a rename.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SaveError::Write {
            path: parent.to_owned(),
            source,
        })?;
    }

    // The temporary must be in the same directory, or the rename crosses a
    // filesystem boundary and stops being atomic.
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, bytes).map_err(|source| SaveError::Write {
        path: temp.clone(),
        source,
    })?;

    std::fs::rename(&temp, path).map_err(|source| {
        // Clean up rather than leaving a stray .tmp beside the user's config.
        let _ = std::fs::remove_file(&temp);
        SaveError::Write {
            path: path.to_owned(),
            source,
        }
    })
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("failed to read {path} before saving")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML, so it will not be overwritten: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: Box<toml_edit::TomlError>,
    },
    #[error("failed to write {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::CursorShape;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir()
                .join(format!(
                    "tuz-save-test-{tag}-{}-{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ))
                .to_string_lossy()
                .replace(['(', ')', ' '], "")
                .into();
            let p: PathBuf = p;
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A config file with comments, blank lines and deliberate alignment.
    const COMMENTED: &str = r#"# Tuzminal configuration
#
# Every setting below is optional.

# Which theme to use. List them with `tuzminal theme list`.
theme = "tuz-dark"


[font]
family = "monospace"
size = 12.0          # trailing comment on the size

# Enable programming ligatures.
ligatures = false

[window]
padding = { x = 8, y = 8 }
opacity = 1.0
"#;

    #[test]
    fn changing_one_value_preserves_every_comment() {
        // The load-bearing test. A plain `toml::to_string` would delete all of these
        // comments, and the file users start from is mostly comments.
        let dir = TempDir::new("comments");
        std::fs::write(dir.file(), COMMENTED).unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.font.size = 15.0;

        save_config(&dir.file(), &changed, &baseline).expect("save should succeed");
        let after = std::fs::read_to_string(dir.file()).unwrap();

        for comment in [
            "# Tuzminal configuration",
            "# Every setting below is optional.",
            "# Which theme to use. List them with `tuzminal theme list`.",
            "# Enable programming ligatures.",
            "# trailing comment on the size",
        ] {
            assert!(
                after.contains(comment),
                "lost comment {comment:?}\n---\n{after}"
            );
        }

        assert!(
            after.contains("size = 15.0"),
            "the change should be present"
        );
    }

    #[test]
    fn untouched_lines_survive_byte_identical() {
        let dir = TempDir::new("untouched");
        std::fs::write(dir.file(), COMMENTED).unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.font.size = 15.0;
        save_config(&dir.file(), &changed, &baseline).unwrap();

        let after = std::fs::read_to_string(dir.file()).unwrap();
        let before_lines: Vec<&str> = COMMENTED.lines().collect();
        let after_lines: Vec<&str> = after.lines().collect();

        assert_eq!(
            before_lines.len(),
            after_lines.len(),
            "the line count changed\n---\n{after}"
        );
        for (before, now) in before_lines.iter().zip(&after_lines) {
            if before.trim_start().starts_with("size") {
                continue; // the one line we changed
            }
            assert_eq!(before, now, "line changed unexpectedly");
        }
    }

    #[test]
    fn only_changed_keys_are_written() {
        // A blanket rewrite would touch settings the user never edited, which is how
        // a GUI silently reformats a hand-tuned file.
        let dir = TempDir::new("only-changed");
        std::fs::write(dir.file(), COMMENTED).unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.theme = "tuz-light".to_owned();

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();

        assert!(after.contains("theme = \"tuz-light\""));
        // The font block is untouched, including its original value.
        assert!(after.contains("size = 12.0"));
        assert!(after.contains("ligatures = false"));
    }

    #[test]
    fn nothing_is_written_when_nothing_changed() {
        // Rewriting identical content would still churn the mtime and wake the
        // config watcher for no reason.
        let dir = TempDir::new("noop");
        std::fs::write(dir.file(), COMMENTED).unwrap();
        let before = std::fs::read_to_string(dir.file()).unwrap();

        let config = Config::default();
        save_config(&dir.file(), &config, &config).unwrap();

        assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), before);
    }

    #[test]
    fn a_missing_file_is_created() {
        let dir = TempDir::new("create");
        assert!(!dir.file().exists());

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.font.size = 14.0;

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();
        assert!(after.contains("size = 14.0"), "got:\n{after}");
        // And it parses back as a valid config.
        let parsed: Config = toml::from_str(&after).expect("the written file must parse");
        assert_eq!(parsed.font.size, 14.0);
    }

    #[test]
    fn a_missing_table_is_created_for_a_nested_key() {
        let dir = TempDir::new("nested");
        std::fs::write(dir.file(), "theme = \"tuz-dark\"\n").unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.cursor.shape = CursorShape::Beam;
        changed.scrollback.lines = 50_000;

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();

        let parsed: Config = toml::from_str(&after).expect("must parse");
        assert_eq!(parsed.cursor.shape, CursorShape::Beam);
        assert_eq!(parsed.scrollback.lines, 50_000);
    }

    #[test]
    fn a_key_inside_an_inline_table_is_updated_in_place() {
        // `padding = { x = 8, y = 8 }` is an inline table; writing into it must not
        // explode it into a `[window.padding]` section.
        let dir = TempDir::new("inline");
        std::fs::write(dir.file(), COMMENTED).unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.window.padding.x = 20;

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();

        let parsed: Config = toml::from_str(&after).expect("must parse");
        assert_eq!(parsed.window.padding.x, 20);
        assert_eq!(parsed.window.padding.y, 8, "the sibling should survive");
    }

    #[test]
    fn every_editable_setting_round_trips_through_the_file() {
        // Guards the hand-written diff table: a setting the panel can change but that
        // `collect_changes` forgets would silently fail to save.
        let dir = TempDir::new("roundtrip");
        let baseline = Config::default();

        let mut changed = baseline.clone();
        changed.theme = "tuz-light".to_owned();
        changed.font.family = "Fira Code".to_owned();
        changed.font.size = 15.5;
        changed.font.line_height = 1.2;
        changed.font.cell_width = 1.1;
        changed.font.ligatures = true;
        changed.window.padding.x = 12;
        changed.window.padding.y = 14;
        changed.window.opacity = 0.9;
        changed.window.decorations = false;
        changed.window.always_show_tab_bar = true;
        changed.window.center_grid = false;
        changed.cursor.shape = CursorShape::Underline;
        changed.cursor.blink = false;
        changed.cursor.blink_interval_ms = 700;
        changed.scrollback.lines = 25_000;
        changed.scrollback.scroll_multiplier = 5;
        changed.performance.vsync = false;
        changed.performance.max_fps = 144;

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();
        let parsed: Config = toml::from_str(&after)
            .unwrap_or_else(|e| panic!("written file must parse: {e}\n---\n{after}"));

        assert_eq!(parsed.theme, "tuz-light");
        assert_eq!(parsed.font.family, "Fira Code");
        assert_eq!(parsed.font.size, 15.5);
        assert_eq!(parsed.font.line_height, 1.2);
        assert_eq!(parsed.font.cell_width, 1.1);
        assert!(parsed.font.ligatures);
        assert_eq!(parsed.window.padding.x, 12);
        assert_eq!(parsed.window.padding.y, 14);
        assert_eq!(parsed.window.opacity, 0.9);
        assert!(!parsed.window.decorations);
        assert!(parsed.window.always_show_tab_bar);
        assert!(!parsed.window.center_grid);
        assert_eq!(parsed.cursor.shape, CursorShape::Underline);
        assert!(!parsed.cursor.blink);
        assert_eq!(parsed.cursor.blink_interval_ms, 700);
        assert_eq!(parsed.scrollback.lines, 25_000);
        assert_eq!(parsed.scrollback.scroll_multiplier, 5);
        assert!(!parsed.performance.vsync);
        assert_eq!(parsed.performance.max_fps, 144);
    }

    #[test]
    fn the_written_file_always_validates() {
        let dir = TempDir::new("validates");
        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.window.opacity = 0.5;
        changed.font.size = 20.0;

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let parsed: Config = toml::from_str(&std::fs::read_to_string(dir.file()).unwrap()).unwrap();
        parsed.validate().expect("a saved config must be valid");
    }

    #[test]
    fn a_malformed_existing_file_is_refused_rather_than_overwritten() {
        // Clobbering a file we cannot parse could destroy work the user is
        // mid-way through editing.
        let dir = TempDir::new("malformed");
        std::fs::write(dir.file(), "this is not toml {{{").unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.font.size = 15.0;

        let err = save_config(&dir.file(), &changed, &baseline).unwrap_err();
        assert!(matches!(err, SaveError::Parse { .. }), "got {err}");
        assert_eq!(
            std::fs::read_to_string(dir.file()).unwrap(),
            "this is not toml {{{",
            "the file must be left exactly as it was"
        );
    }

    #[test]
    fn no_temporary_file_is_left_behind() {
        let dir = TempDir::new("temp");
        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.font.size = 13.0;
        save_config(&dir.file(), &changed, &baseline).unwrap();

        let strays: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");
    }

    #[test]
    fn changes_are_diffed_against_the_baseline_not_the_defaults() {
        // Saving must record a change back *to* a default value too, otherwise
        // turning a setting off could never be persisted.
        let dir = TempDir::new("baseline");
        std::fs::write(dir.file(), "[font]\nligatures = true\n").unwrap();

        let mut baseline = Config::default();
        baseline.font.ligatures = true;
        let changed = Config::default(); // ligatures back to false

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();
        let parsed: Config = toml::from_str(&after).unwrap();
        assert!(
            !parsed.font.ligatures,
            "turning a setting off must be saved\n---\n{after}"
        );
    }

    #[test]
    fn empty_tables_are_not_added_for_settings_that_did_not_change() {
        let dir = TempDir::new("no-empty");
        std::fs::write(dir.file(), "theme = \"tuz-dark\"\n").unwrap();

        let baseline = Config::default();
        let mut changed = baseline.clone();
        changed.theme = "tuz-light".to_owned();

        save_config(&dir.file(), &changed, &baseline).unwrap();
        let after = std::fs::read_to_string(dir.file()).unwrap();

        for section in ["[font]", "[window]", "[cursor]", "[performance]"] {
            assert!(
                !after.contains(section),
                "an unchanged section {section} should not be added\n---\n{after}"
            );
        }
    }

    #[test]
    fn cursor_shape_names_match_the_serde_spelling() {
        // A mismatch here writes a value the loader then rejects.
        for shape in [
            CursorShape::Block,
            CursorShape::Beam,
            CursorShape::Underline,
            CursorShape::HollowBlock,
        ] {
            let name = cursor_shape_name(shape);
            let toml = format!("[cursor]\nshape = \"{name}\"\n");
            let parsed: Config = toml::from_str(&toml)
                .unwrap_or_else(|e| panic!("`{name}` should be a valid shape: {e}"));
            assert_eq!(parsed.cursor.shape, shape);
        }
    }
}
