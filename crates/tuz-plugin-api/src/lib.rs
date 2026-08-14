//! The contract between Tuzminal and its plugins.
//!
//! This crate is the **single source of truth** for what a plugin can see and do.
//! Both the Lua and the WebAssembly runtimes are thin adapters over these types,
//! so a plugin's capabilities never depend on which language wrote it.
//!
//! # Why message passing
//!
//! Plugins never touch renderer or terminal state. They receive read-only
//! [`Event`] snapshots and emit [`Command`]s onto a queue the main thread drains.
//! Three things fall out of that choice:
//!
//! - one API can serve two runtimes, because everything crossing the boundary is
//!   serializable;
//! - a misbehaving plugin can be aborted mid-callback without leaving anything
//!   half-mutated;
//! - the host keeps full control over ordering, so a plugin cannot interleave
//!   itself into the middle of a frame.
//!
//! # Versioning
//!
//! [`API_VERSION`] is the major version of this contract. A plugin declares the
//! version it was written against in its manifest, and the host refuses to load a
//! plugin whose major version differs. Adding a variant to [`Event`] or
//! [`Command`] is a breaking change for the WASM ABI, so both enums are
//! `#[non_exhaustive]` and new variants must go at the end.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Major version of the plugin contract.
///
/// Bumped only for breaking changes. A plugin declaring a different major version
/// is refused rather than loaded and allowed to misbehave in confusing ways.
pub const API_VERSION: u32 = 1;

/// Identifies a pane. Opaque to plugins; ids are never reused, so a stale id is
/// detectably invalid rather than silently pointing at a different pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PaneId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pane{}", self.0)
    }
}

/// A direction for splits and focus movement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// A modifier combination, as a plugin sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
}

/// A key press delivered to `on_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPress {
    /// Canonical chord text, e.g. `ctrl+shift+d`. Matching on this is stable
    /// across keyboard layouts.
    pub chord: String,
    pub modifiers: Modifiers,
}

// ---------------------------------------------------------------------------
// Events: host -> plugin
// ---------------------------------------------------------------------------

/// Something that happened, delivered to a plugin's handler.
///
/// New variants are appended, never inserted, because the WASM ABI encodes the
/// discriminant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// Delivered once after the plugin loads. Register keybinds and commands here.
    Startup,

    /// Configuration was reloaded from disk.
    ConfigReload,

    /// A key was pressed. The plugin may claim it by returning
    /// [`KeyOutcome::Handled`], which stops the terminal from acting on it.
    ///
    /// Answered synchronously under a deadline, so handlers must be trivial.
    Key(KeyPress),

    /// A pane produced output. **Opt-in** via the `read-output` permission,
    /// because delivering every byte a shell writes is expensive and lets a plugin
    /// see everything the user does.
    PaneOutput {
        pane: PaneId,
        text: String,
    },

    /// The focused tab changed.
    TabSwitch {
        index: u32,
    },

    /// A pane's reported title changed.
    TitleChange {
        pane: PaneId,
        title: String,
    },

    /// A pane rang the bell.
    Bell {
        pane: PaneId,
    },

    /// A pane was created or destroyed.
    PaneOpened {
        pane: PaneId,
    },
    PaneClosed {
        pane: PaneId,
    },

    /// An OSC sequence the terminal did not consume itself. Lets plugins define
    /// their own escape-sequence protocols.
    Osc {
        pane: PaneId,
        code: u16,
        payload: String,
    },

    /// The status bar is about to be drawn and wants segments.
    StatusBarRender,

    /// A status segment this plugin published was clicked.
    ///
    /// Only segments given an `id` are clickable; the rest are drawn and ignored,
    /// which keeps a clock from swallowing a press meant for the window behind it.
    StatusSegmentClick {
        id: String,
    },

    /// A command the plugin registered was invoked.
    Command {
        name: String,
        args: Vec<String>,
    },

    /// The focused pane's input line changed: the cursor row's text, up to the
    /// cursor.
    ///
    /// **Opt-in twice** — an `input_line` entry in `events` *and* the `read-input`
    /// permission — because this is what the user is typing.
    ///
    /// Delivered only for the focused pane, only when it actually moved, and never
    /// while a full-screen program holds the alternate screen, so what is typed into
    /// `vim` or a TUI is not reported. It is not filtered beyond that: a secret
    /// typed as a command-line argument is echoed by the shell, so it is in here.
    ///
    /// `line` starts at column 0, which means it **includes the shell's prompt**. No
    /// escape sequence marks where a prompt ends, so the host does not pretend to
    /// know; a plugin matching against history has to work that out for itself.
    InputLine {
        pane: PaneId,
        /// Row text left of the cursor. Wide-glyph spacers are dropped, so one
        /// glyph is one `char`.
        line: String,
        /// The cursor's column — *not* `line.chars().count()`, because a
        /// double-width glyph is one `char` and two columns.
        cursor_col: u16,
        /// Whether the rest of the row is empty, i.e. the cursor is at the end of
        /// what is written rather than somewhere inside it.
        ///
        /// Reported rather than left to be inferred, because a plugin cannot work it
        /// out: `line` stops at the cursor, and counting columns against `char`s
        /// breaks on wide glyphs. It is the condition for appending anything to the
        /// line — a suggestion offered mid-command would corrupt it — so guessing
        /// would be guessing about whether it is safe to type.
        at_line_end: bool,
    },
}

/// A plugin's answer to [`Event::Key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyOutcome {
    /// The terminal should handle the key as usual.
    #[default]
    Unhandled,
    /// The plugin consumed the key; the terminal must not act on it.
    Handled,
}

// ---------------------------------------------------------------------------
// Commands: plugin -> host
// ---------------------------------------------------------------------------

/// One segment of the status bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSegment {
    pub text: String,
    /// Identifier sent back in [`Event::StatusSegmentClick`] when this segment is
    /// pressed. `None` makes the segment inert, which is what a clock wants.
    ///
    /// A plugin's own id, not a global one: the host qualifies it with the plugin
    /// name before dispatch, so two plugins can both use `"open"` without colliding.
    #[serde(default)]
    pub id: Option<String>,
    /// `#rrggbb`, or `None` for the theme's default.
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
}

/// Something a plugin asks the terminal to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command {
    Split {
        direction: Direction,
    },
    NewTab,
    ClosePane {
        pane: Option<PaneId>,
    },
    Focus {
        direction: Direction,
    },
    FocusPane {
        pane: PaneId,
    },
    SelectTab {
        index: u32,
    },

    /// Resize the focused split by a fraction, positive meaning right or down.
    Resize {
        direction: Direction,
        delta: f32,
    },

    /// Write bytes to a pane as if typed. `None` targets the focused pane.
    SendText {
        pane: Option<PaneId>,
        text: String,
    },

    /// Show a transient message to the user.
    Notify {
        message: String,
        level: NotifyLevel,
    },

    /// Bind a chord to one of this plugin's commands.
    RegisterKeybind {
        chord: String,
        command: String,
    },

    /// Declare a command name so it can be bound in config and invoked.
    RegisterCommand {
        name: String,
        description: String,
    },

    /// Replace this plugin's status bar segments.
    SetStatusSegments {
        segments: Vec<StatusSegment>,
    },

    /// Overlay configuration values, as a TOML fragment.
    ///
    /// A fragment rather than typed fields so plugins keep working when the config
    /// schema grows. Invalid fragments are rejected and reported, never partially
    /// applied.
    SetConfigOverlay {
        toml: String,
    },

    /// Ask the terminal to reload configuration from disk.
    ReloadConfig,

    Quit,

    /// Show dim "ghost" text at a pane's cursor. Empty `text` clears it.
    ///
    /// The one thing a plugin can draw, and deliberately the narrowest exception
    /// that makes history autosuggestion possible: the plugin supplies a string and
    /// nothing else. The host chooses the position (the cursor) and the colour (the
    /// theme's), refuses the hint whenever it would cover something the program
    /// printed, refuses it on the alternate screen and on an unfocused pane,
    /// truncates it at the last column, and drops it as soon as the line changes.
    ///
    /// `None` targets the focused pane. A hint on an unfocused pane is stored and
    /// never drawn — a suggestion on a pane you cannot type into would be a lie — so
    /// the field exists to keep the two runtimes' reach equal rather than for use.
    SetInlineHint {
        pane: Option<PaneId>,
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    #[default]
    Info,
    Warn,
    Error,
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

/// Which runtime executes a plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Lua,
    Wasm,
}

/// A capability a plugin must declare to use.
///
/// Enforcement differs by runtime and the difference is not cosmetic: WASM
/// permissions are structural (an ungranted host function is never linked, so it
/// cannot be called), while Lua permissions are a restricted environment that a
/// determined plugin could work around. See [`Permission::is_security_boundary`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permission {
    /// Receive [`Event::PaneOutput`]. Grants sight of everything the user does.
    ReadOutput,
    /// Receive [`Event::InputLine`]. Grants sight of what is typed at the prompt.
    ReadInput,
    /// Spawn external processes.
    SpawnProcess,
    /// Make network requests.
    Network,
    /// Read a filesystem path.
    #[serde(rename = "fs-read")]
    FsRead(String),
    /// Write a filesystem path.
    #[serde(rename = "fs-write")]
    FsWrite(String),
    /// Read and write the clipboard.
    Clipboard,
}

impl Permission {
    /// Whether granting this permission is enforceable as a security boundary.
    ///
    /// Only meaningful for WASM plugins. For Lua the answer is always no, and the
    /// host says so at load time rather than implying a guarantee it cannot keep.
    pub fn is_security_boundary(&self, runtime: Runtime) -> bool {
        matches!(runtime, Runtime::Wasm)
    }

    /// Human-readable description, for the install prompt.
    pub fn describe(&self) -> String {
        match self {
            Permission::ReadOutput => {
                "see all terminal output, including passwords you type".to_owned()
            }
            // Deliberately not the same sentence as `ReadOutput`: this is one row
            // left of the cursor, not the whole session. Overstating a grant is how
            // users learn to click through permission prompts without reading them.
            Permission::ReadInput => {
                "see the command line you are typing, including secrets passed as arguments"
                    .to_owned()
            }
            Permission::SpawnProcess => "run other programs".to_owned(),
            Permission::Network => "make network requests".to_owned(),
            Permission::FsRead(p) => format!("read files under {p}"),
            Permission::FsWrite(p) => format!("write files under {p}"),
            Permission::Clipboard => "read and change the clipboard".to_owned(),
        }
    }
}

/// A plugin's `plugin.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// Major version of the plugin contract this plugin targets.
    pub api_version: u32,
    pub runtime: Runtime,
    /// Entry point, relative to the plugin directory.
    pub entry: String,

    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub homepage: String,

    /// Capabilities the plugin needs. Anything not listed is denied.
    #[serde(default)]
    pub permissions: Vec<Permission>,

    /// Events the plugin wants. Empty means "all cheap events"; expensive ones
    /// like `pane_output` must always be requested explicitly.
    #[serde(default)]
    pub events: Vec<String>,

    /// Plugin-specific settings, passed through untouched.
    #[serde(default)]
    pub config: BTreeMap<String, toml::Value>,
}

impl Manifest {
    /// Parse and validate a manifest.
    pub fn parse(src: &str) -> Result<Self, ManifestError> {
        let manifest: Manifest =
            toml::from_str(src).map_err(|e| ManifestError::Syntax(Box::new(e)))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyName);
        }
        // The name becomes a directory and a command prefix, so restrict it before
        // it can be used to escape either.
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ManifestError::InvalidName(self.name.clone()));
        }
        if self.api_version != API_VERSION {
            return Err(ManifestError::ApiVersion {
                plugin: self.name.clone(),
                found: self.api_version,
                expected: API_VERSION,
            });
        }
        if self.entry.trim().is_empty() {
            return Err(ManifestError::EmptyEntry);
        }
        // A traversing entry path would let a plugin load code from outside its own
        // directory.
        if self.entry.contains("..") || self.entry.starts_with('/') {
            return Err(ManifestError::InvalidEntry(self.entry.clone()));
        }

        let expected_ext = match self.runtime {
            Runtime::Lua => "lua",
            Runtime::Wasm => "wasm",
        };
        if !self.entry.ends_with(expected_ext) {
            return Err(ManifestError::EntryExtension {
                entry: self.entry.clone(),
                expected: expected_ext,
            });
        }
        Ok(())
    }

    /// Whether the plugin asked for an event by name.
    ///
    /// An empty `events` list means the cheap defaults. Some events are never a
    /// default: they cost something to deliver and they show a plugin what the user
    /// is doing, so both the intent (`events`) and the grant (`permissions`) have to
    /// be explicit.
    ///
    /// A table rather than an `if` chain, so a third gated event is one line instead
    /// of a second special case.
    pub fn wants_event(&self, name: &str) -> bool {
        let gated = match name {
            "pane_output" => Some(Permission::ReadOutput),
            "input_line" => Some(Permission::ReadInput),
            _ => None,
        };
        if let Some(required) = gated {
            return self.events.iter().any(|e| e == name) && self.permissions.contains(&required);
        }
        self.events.is_empty() || self.events.iter().any(|e| e == name)
    }

    pub fn has_permission(&self, permission: &Permission) -> bool {
        self.permissions.contains(permission)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin.toml is not valid TOML: {0}")]
    Syntax(Box<toml::de::Error>),
    #[error("plugin name must not be empty")]
    EmptyName,
    #[error("plugin name `{0}` may only contain letters, digits, `-` and `_`")]
    InvalidName(String),
    #[error("plugin `{plugin}` targets API version {found}, but this build provides {expected}")]
    ApiVersion {
        plugin: String,
        found: u32,
        expected: u32,
    },
    #[error("plugin entry must not be empty")]
    EmptyEntry,
    #[error("plugin entry `{0}` must be a relative path inside the plugin directory")]
    InvalidEntry(String),
    #[error("plugin entry `{entry}` should have the `{expected}` extension for its runtime")]
    EntryExtension {
        entry: String,
        expected: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_toml(extra: &str) -> String {
        format!(
            r#"
name = "example"
version = "0.1.0"
api_version = {API_VERSION}
runtime = "lua"
entry = "init.lua"
{extra}
"#
        )
    }

    #[test]
    fn a_minimal_manifest_parses() {
        let m = Manifest::parse(&manifest_toml("")).unwrap();
        assert_eq!(m.name, "example");
        assert_eq!(m.runtime, Runtime::Lua);
        assert!(m.permissions.is_empty());
    }

    #[test]
    fn a_mismatched_api_version_is_refused() {
        // Loading a plugin built for a different contract would fail in confusing
        // ways later; refusing up front names the real problem.
        let src =
            manifest_toml("").replace(&format!("api_version = {API_VERSION}"), "api_version = 999");
        let err = Manifest::parse(&src).unwrap_err();
        assert!(matches!(err, ManifestError::ApiVersion { .. }));
        assert!(err.to_string().contains("999"));
    }

    #[test]
    fn names_that_could_escape_a_directory_are_rejected() {
        for bad in ["../evil", "has space", "semi;colon", "sub/dir"] {
            let src = manifest_toml("").replace("name = \"example\"", &format!("name = \"{bad}\""));
            assert!(
                matches!(Manifest::parse(&src), Err(ManifestError::InvalidName(_))),
                "`{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn entry_paths_cannot_traverse_out_of_the_plugin_directory() {
        for bad in ["../../etc/passwd.lua", "/etc/passwd.lua"] {
            let src =
                manifest_toml("").replace("entry = \"init.lua\"", &format!("entry = \"{bad}\""));
            assert!(
                matches!(Manifest::parse(&src), Err(ManifestError::InvalidEntry(_))),
                "`{bad}` should be rejected"
            );
        }
    }

    #[test]
    fn the_entry_extension_must_match_the_runtime() {
        // A wasm runtime pointed at a .lua file is a packaging mistake worth
        // catching at load time rather than as a confusing parse failure.
        let src = manifest_toml("").replace("runtime = \"lua\"", "runtime = \"wasm\"");
        assert!(matches!(
            Manifest::parse(&src),
            Err(ManifestError::EntryExtension { .. })
        ));
    }

    #[test]
    fn permissions_parse_from_kebab_case() {
        let m = Manifest::parse(&manifest_toml(
            r#"permissions = ["read-output", "clipboard", { fs-read = "/tmp" }]"#,
        ))
        .unwrap();

        assert!(m.has_permission(&Permission::ReadOutput));
        assert!(m.has_permission(&Permission::Clipboard));
        assert!(m.has_permission(&Permission::FsRead("/tmp".to_owned())));
        assert!(!m.has_permission(&Permission::Network));
    }

    #[test]
    fn pane_output_requires_both_the_event_and_the_permission() {
        // Either alone must not be enough: the event is expensive and the data is
        // sensitive, so both the intent and the grant have to be explicit.
        let neither = Manifest::parse(&manifest_toml("")).unwrap();
        assert!(!neither.wants_event("pane_output"));

        let event_only = Manifest::parse(&manifest_toml(r#"events = ["pane_output"]"#)).unwrap();
        assert!(!event_only.wants_event("pane_output"));

        let permission_only =
            Manifest::parse(&manifest_toml(r#"permissions = ["read-output"]"#)).unwrap();
        assert!(!permission_only.wants_event("pane_output"));

        let both = Manifest::parse(&manifest_toml(
            "events = [\"pane_output\"]\npermissions = [\"read-output\"]",
        ))
        .unwrap();
        assert!(both.wants_event("pane_output"));
    }

    #[test]
    fn input_line_requires_both_the_event_and_the_permission() {
        // Same double opt-in as `pane_output`, and for the same reason: the event is
        // what the user is typing, so neither the intent nor the grant alone is
        // enough.
        let neither = Manifest::parse(&manifest_toml("")).unwrap();
        assert!(!neither.wants_event("input_line"));

        let event_only = Manifest::parse(&manifest_toml(r#"events = ["input_line"]"#)).unwrap();
        assert!(!event_only.wants_event("input_line"));

        let permission_only =
            Manifest::parse(&manifest_toml(r#"permissions = ["read-input"]"#)).unwrap();
        assert!(!permission_only.wants_event("input_line"));

        let both = Manifest::parse(&manifest_toml(
            "events = [\"input_line\"]\npermissions = [\"read-input\"]",
        ))
        .unwrap();
        assert!(both.wants_event("input_line"));
    }

    #[test]
    fn read_output_does_not_grant_the_input_line_and_the_reverse() {
        // The two permissions are deliberately separate so either can be granted
        // without the other; a plugin wanting suggestions must not have to ask for
        // sight of the whole session.
        let output = Manifest::parse(&manifest_toml(
            "events = [\"input_line\"]\npermissions = [\"read-output\"]",
        ))
        .unwrap();
        assert!(!output.wants_event("input_line"));

        let input = Manifest::parse(&manifest_toml(
            "events = [\"pane_output\"]\npermissions = [\"read-input\"]",
        ))
        .unwrap();
        assert!(!input.wants_event("pane_output"));
    }

    #[test]
    fn read_input_and_read_output_do_not_describe_the_same_thing() {
        // These strings are what the installer shows. `read-input` is one row left of
        // the cursor, not the session; describing it with `read-output`'s sentence
        // would overstate the grant, and prompts that overstate get clicked through.
        let input = Permission::ReadInput.describe();
        let output = Permission::ReadOutput.describe();
        assert_ne!(input, output);
        assert!(input.contains("typing"), "got: {input}");
    }

    #[test]
    fn an_empty_event_list_means_the_cheap_defaults() {
        let m = Manifest::parse(&manifest_toml("")).unwrap();
        assert!(m.wants_event("startup"));
        assert!(m.wants_event("bell"));
        // But never the expensive ones.
        assert!(!m.wants_event("pane_output"));
        assert!(!m.wants_event("input_line"));
    }

    #[test]
    fn an_explicit_event_list_excludes_everything_else() {
        let m = Manifest::parse(&manifest_toml(r#"events = ["startup"]"#)).unwrap();
        assert!(m.wants_event("startup"));
        assert!(!m.wants_event("bell"));
    }

    #[test]
    fn unknown_manifest_keys_are_errors() {
        let err = Manifest::parse(&manifest_toml("permisions = []")).unwrap_err();
        assert!(matches!(err, ManifestError::Syntax(_)), "{err}");
    }

    #[test]
    fn only_wasm_permissions_are_a_security_boundary() {
        // The docs promise this distinction; assert it so the promise cannot drift.
        let p = Permission::Network;
        assert!(p.is_security_boundary(Runtime::Wasm));
        assert!(
            !p.is_security_boundary(Runtime::Lua),
            "Lua sandboxing is best-effort and must not claim otherwise"
        );
    }

    #[test]
    fn permission_descriptions_are_honest_about_read_output() {
        // This string is what a user sees when deciding whether to install; it must
        // name the actual risk.
        let text = Permission::ReadOutput.describe();
        assert!(text.contains("password"), "got: {text}");
    }

    #[test]
    fn events_and_commands_round_trip_through_json_like_serde() {
        // The WASM boundary serializes these, so a variant that cannot round-trip
        // would fail only at runtime, inside a plugin.
        let events = vec![
            Event::Startup,
            Event::Key(KeyPress {
                chord: "ctrl+shift+p".to_owned(),
                modifiers: Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
            }),
            Event::PaneOutput {
                pane: PaneId(3),
                text: "hi".to_owned(),
            },
            Event::Osc {
                pane: PaneId(1),
                code: 777,
                payload: "x".to_owned(),
            },
            Event::InputLine {
                pane: PaneId(2),
                line: "$ git st".to_owned(),
                cursor_col: 8,
                at_line_end: true,
            },
        ];
        for event in events {
            let encoded = toml::to_string(&event).expect("event should serialize");
            let decoded: Event = toml::from_str(&encoded).expect("event should deserialize");
            assert_eq!(decoded, event);
        }

        let commands = vec![
            Command::NewTab,
            Command::Split {
                direction: Direction::Right,
            },
            Command::SendText {
                pane: None,
                text: "ls\n".to_owned(),
            },
            Command::SetStatusSegments {
                segments: vec![StatusSegment {
                    id: None,
                    text: "seg".to_owned(),
                    foreground: Some("#ff0000".to_owned()),
                    background: None,
                }],
            },
            Command::SetInlineHint {
                pane: None,
                text: "atus".to_owned(),
            },
            // The empty-text form is how a plugin says "no suggestion", so it has to
            // survive the round trip too rather than being dropped as a default.
            Command::SetInlineHint {
                pane: Some(PaneId(2)),
                text: String::new(),
            },
        ];
        for command in commands {
            let encoded = toml::to_string(&command).expect("command should serialize");
            let decoded: Command = toml::from_str(&encoded).expect("command should deserialize");
            assert_eq!(decoded, command);
        }
    }

    #[test]
    fn key_outcome_defaults_to_unhandled() {
        // A plugin that returns nothing must not accidentally swallow every key.
        assert_eq!(KeyOutcome::default(), KeyOutcome::Unhandled);
    }
}
