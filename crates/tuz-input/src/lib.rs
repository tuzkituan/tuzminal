//! Keyboard input handling for Tuzminal: chords, actions and keymaps.
//!
//! ```
//! use tuz_input::{Keymap, KeyChord, Action, Modifiers};
//!
//! let built = Keymap::from_config(
//!     [("ctrl+shift+d", "split_right"), ("ctrl+shift+q", "not_an_action")],
//!     &Default::default(),
//! );
//!
//! // Good bindings apply; bad ones are reported without discarding the rest.
//! assert_eq!(built.errors.len(), 1);
//! assert_eq!(
//!     built.keymap.lookup(&KeyChord::parse("ctrl+shift+d").unwrap()),
//!     Some(&Action::SplitRight),
//! );
//! # let _ = Modifiers::CTRL;
//! ```

pub mod action;
pub mod chord;

pub use action::{Action, UNBIND};
pub use chord::{ChordParseError, Key, KeyChord, Modifiers, NamedKey};

use std::collections::{HashMap, HashSet};

/// Chord-to-action bindings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Keymap {
    bindings: HashMap<KeyChord, Action>,
}

/// A binding that could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// The chord side failed to parse.
    Chord(ChordParseError),
    /// The action name is not a builtin and no plugin registered it.
    UnknownAction { chord: String, action: String },
    /// Two entries bind the same chord after normalization.
    Conflict {
        chord: String,
        existing: String,
        replacement: String,
    },
}

impl std::fmt::Display for BindingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindingError::Chord(e) => write!(f, "{e}"),
            BindingError::UnknownAction { chord, action } => write!(
                f,
                "binding `{chord}`: unknown action `{action}` \
                 (see `tuzminal --list-actions`)"
            ),
            BindingError::Conflict {
                chord,
                existing,
                replacement,
            } => write!(
                f,
                "binding `{chord}` is defined twice: `{existing}` then \
                 `{replacement}`; the later one wins"
            ),
        }
    }
}

impl std::error::Error for BindingError {}

/// A keymap plus whatever went wrong building it.
#[derive(Debug, Clone, Default)]
pub struct BuiltKeymap {
    pub keymap: Keymap,
    /// Non-fatal problems. One bad line must not cost the user every other
    /// binding, so these are collected and surfaced rather than returned as a
    /// hard error.
    pub errors: Vec<BindingError>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a keymap from config entries.
    ///
    /// `plugin_actions` holds command names registered by loaded plugins; a name
    /// found there resolves to [`Action::Plugin`]. Anything else unrecognized is
    /// reported as an error. Binding an action to [`UNBIND`] removes the chord,
    /// which is how a user turns off a default.
    pub fn from_config<'a, I>(entries: I, plugin_actions: &HashSet<String>) -> BuiltKeymap
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let mut keymap = Keymap::new();
        let mut errors = Vec::new();
        // Tracks what each chord was last bound to, for conflict reporting.
        let mut sources: HashMap<KeyChord, String> = HashMap::new();

        for (chord_str, action_str) in entries {
            let chord = match KeyChord::parse(chord_str) {
                Ok(c) => c,
                Err(e) => {
                    errors.push(BindingError::Chord(e));
                    continue;
                }
            };

            if action_str.trim().eq_ignore_ascii_case(UNBIND) {
                keymap.bindings.remove(&chord);
                sources.remove(&chord);
                continue;
            }

            let action = match Action::parse(action_str) {
                Some(a) => a,
                None if plugin_actions.contains(action_str.trim()) => {
                    Action::Plugin(action_str.trim().to_owned())
                }
                None => {
                    errors.push(BindingError::UnknownAction {
                        chord: chord.to_string(),
                        action: action_str.to_owned(),
                    });
                    continue;
                }
            };

            // Two spellings of one chord (`shift+ctrl+d` and `ctrl+shift+d`)
            // normalize to the same key, so silently dropping one would be
            // baffling to debug.
            if let Some(existing) = sources.get(&chord) {
                errors.push(BindingError::Conflict {
                    chord: chord.to_string(),
                    existing: existing.clone(),
                    replacement: action.to_string(),
                });
            }
            sources.insert(chord, action.to_string());
            keymap.bindings.insert(chord, action);
        }

        BuiltKeymap { keymap, errors }
    }

    /// Look up a chord. Call [`KeyChord::normalized`] on chords built from live
    /// key events first.
    pub fn lookup(&self, chord: &KeyChord) -> Option<&Action> {
        self.bindings.get(chord)
    }

    /// Resolve a raw key event straight to an action.
    pub fn resolve(&self, mods: Modifiers, key: Key) -> Option<&Action> {
        self.lookup(&KeyChord::new(mods, key).normalized())
    }

    pub fn bind(&mut self, chord: KeyChord, action: Action) -> Option<Action> {
        self.bindings.insert(chord.normalized(), action)
    }

    pub fn unbind(&mut self, chord: &KeyChord) -> Option<Action> {
        self.bindings.remove(&chord.normalized())
    }

    pub fn len(&self) -> usize {
        self.bindings.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// All bindings, sorted for stable display in help output.
    ///
    /// Sorted by rendered chord rather than by the internal modifier bitset:
    /// both are deterministic, but only one produces a list a human can scan.
    pub fn iter_sorted(&self) -> Vec<(KeyChord, &Action)> {
        let mut v: Vec<(KeyChord, &Action)> = self.bindings.iter().map(|(c, a)| (*c, a)).collect();
        v.sort_by_cached_key(|(c, _)| c.to_string());
        v
    }

    /// Chords bound to a given action, for showing shortcuts in a UI.
    pub fn chords_for(&self, action: &Action) -> Vec<KeyChord> {
        let mut v: Vec<KeyChord> = self
            .bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(c, _)| *c)
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_plugins() -> HashSet<String> {
        HashSet::new()
    }

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn builds_bindings_from_config() {
        let built = Keymap::from_config(
            [
                ("ctrl+shift+d", "split_right"),
                ("ctrl+shift+t", "new_tab"),
                ("shift+pageup", "scroll_page_up"),
            ],
            &no_plugins(),
        );

        assert!(built.errors.is_empty(), "{:?}", built.errors);
        assert_eq!(built.keymap.len(), 3);
        assert_eq!(
            built.keymap.lookup(&chord("ctrl+shift+d")),
            Some(&Action::SplitRight)
        );
    }

    #[test]
    fn one_bad_binding_does_not_discard_the_others() {
        // The whole reason errors are collected instead of returned: a typo in
        // one line must not leave the user with an unusable keyboard.
        let built = Keymap::from_config(
            [
                ("ctrl+shift+d", "split_right"),
                ("ctrl+shift+z", "explode"),
                ("nonsense+q", "quit"),
                ("ctrl+shift+t", "new_tab"),
            ],
            &no_plugins(),
        );

        assert_eq!(built.errors.len(), 2);
        assert_eq!(built.keymap.len(), 2, "good bindings must survive");
        assert!(built.keymap.lookup(&chord("ctrl+shift+t")).is_some());
    }

    #[test]
    fn unknown_action_names_are_reported_with_the_chord() {
        let built = Keymap::from_config([("ctrl+shift+z", "explode")], &no_plugins());
        match &built.errors[0] {
            BindingError::UnknownAction { chord, action } => {
                assert_eq!(chord, "ctrl+shift+z");
                assert_eq!(action, "explode");
            }
            other => panic!("expected UnknownAction, got {other:?}"),
        }
        // The message points at how to find the right name.
        assert!(built.errors[0].to_string().contains("--list-actions"));
    }

    #[test]
    fn plugin_registered_actions_resolve() {
        let mut plugins = HashSet::new();
        plugins.insert("statusbar.toggle".to_owned());

        let built = Keymap::from_config([("ctrl+shift+b", "statusbar.toggle")], &plugins);
        assert!(built.errors.is_empty());
        assert_eq!(
            built.keymap.lookup(&chord("ctrl+shift+b")),
            Some(&Action::Plugin("statusbar.toggle".to_owned()))
        );
    }

    #[test]
    fn an_unregistered_plugin_action_is_still_an_error() {
        let built = Keymap::from_config([("ctrl+shift+b", "statusbar.toggle")], &no_plugins());
        assert_eq!(built.errors.len(), 1);
        assert!(built.keymap.is_empty());
    }

    #[test]
    fn none_unbinds_a_chord() {
        let built = Keymap::from_config(
            [("ctrl+shift+d", "split_right"), ("ctrl+shift+d", "none")],
            &no_plugins(),
        );
        assert!(built.keymap.lookup(&chord("ctrl+shift+d")).is_none());
        assert!(
            built.errors.is_empty(),
            "unbinding is intentional, not a conflict: {:?}",
            built.errors
        );
    }

    #[test]
    fn unbinding_is_case_insensitive() {
        let built = Keymap::from_config(
            [("ctrl+shift+d", "split_right"), ("ctrl+shift+d", "NONE")],
            &no_plugins(),
        );
        assert!(built.keymap.is_empty());
    }

    #[test]
    fn later_bindings_win_and_the_conflict_is_reported() {
        let built = Keymap::from_config(
            [("ctrl+shift+d", "split_right"), ("shift+ctrl+d", "new_tab")],
            &no_plugins(),
        );

        // The two spellings normalize to one chord, so the user needs telling.
        assert_eq!(built.errors.len(), 1);
        assert!(matches!(built.errors[0], BindingError::Conflict { .. }));
        assert_eq!(
            built.keymap.lookup(&chord("ctrl+shift+d")),
            Some(&Action::NewTab),
            "the last binding should win"
        );
    }

    #[test]
    fn resolve_normalizes_a_shifted_key_event() {
        let built = Keymap::from_config([("ctrl+shift+d", "split_right")], &no_plugins());
        // A real key event reports the shifted character.
        assert_eq!(
            built.keymap.resolve(Modifiers::CTRL, Key::Char('D')),
            Some(&Action::SplitRight)
        );
        // And the already-normalized form works too.
        assert_eq!(
            built
                .keymap
                .resolve(Modifiers::CTRL | Modifiers::SHIFT, Key::Char('d')),
            Some(&Action::SplitRight)
        );
    }

    #[test]
    fn unbound_chords_resolve_to_nothing() {
        let built = Keymap::from_config([("ctrl+shift+d", "split_right")], &no_plugins());
        assert_eq!(built.keymap.resolve(Modifiers::NONE, Key::Char('a')), None);
        // Plain ctrl+d belongs to the running program, not to us.
        assert_eq!(built.keymap.resolve(Modifiers::CTRL, Key::Char('d')), None);
    }

    #[test]
    fn bind_and_unbind_normalize_their_input() {
        let mut km = Keymap::new();
        km.bind(KeyChord::char(Modifiers::CTRL, 'D'), Action::Quit);
        assert_eq!(km.lookup(&chord("ctrl+shift+d")), Some(&Action::Quit));

        assert_eq!(
            km.unbind(&KeyChord::char(Modifiers::CTRL, 'D')),
            Some(Action::Quit)
        );
        assert!(km.is_empty());
    }

    #[test]
    fn chords_for_finds_every_binding_of_an_action() {
        let built = Keymap::from_config(
            [
                ("ctrl+shift+h", "focus_left"),
                ("ctrl+shift+left", "focus_left"),
                ("ctrl+shift+t", "new_tab"),
            ],
            &no_plugins(),
        );

        let chords = built.keymap.chords_for(&Action::FocusLeft);
        assert_eq!(chords.len(), 2, "both aliases should be listed");
        assert!(chords.contains(&chord("ctrl+shift+h")));
        assert!(chords.contains(&chord("ctrl+shift+left")));
    }

    #[test]
    fn iter_sorted_is_deterministic() {
        // HashMap order is not stable, so help output must be sorted or it
        // reshuffles between runs.
        let entries = [
            ("ctrl+shift+t", "new_tab"),
            ("ctrl+shift+d", "split_right"),
            ("f1", "reload_config"),
        ];
        let a = Keymap::from_config(entries, &no_plugins()).keymap;
        let b = Keymap::from_config(entries, &no_plugins()).keymap;

        let names_a: Vec<String> = a.iter_sorted().iter().map(|(c, _)| c.to_string()).collect();
        let names_b: Vec<String> = b.iter_sorted().iter().map(|(c, _)| c.to_string()).collect();
        assert_eq!(names_a, names_b);
    }

    #[test]
    fn the_shipped_default_keymap_is_free_of_errors() {
        // Guards against a default binding that names a nonexistent action or a
        // chord that does not parse — a broken default would ship silently.
        let defaults = tuz_default_keys();
        let built = Keymap::from_config(defaults.iter().map(|(k, v)| (*k, *v)), &no_plugins());
        assert!(
            built.errors.is_empty(),
            "default keymap has problems: {:?}",
            built.errors
        );
        assert_eq!(built.keymap.len(), defaults.len());
    }

    /// Mirror of the default keymap in `tuz-config`, duplicated here so this
    /// crate stays dependency-free. `tests/default_keymap.rs` in `tuzminal`
    /// asserts the two lists agree.
    fn tuz_default_keys() -> Vec<(&'static str, &'static str)> {
        vec![
            ("ctrl+shift+d", "split_right"),
            ("ctrl+shift+e", "split_down"),
            ("ctrl+shift+w", "close_pane"),
            ("ctrl+shift+t", "new_tab"),
            ("ctrl+shift+h", "focus_left"),
            ("ctrl+shift+j", "focus_down"),
            ("ctrl+shift+k", "focus_up"),
            ("ctrl+shift+l", "focus_right"),
            ("ctrl+shift+left", "focus_left"),
            ("ctrl+shift+down", "focus_down"),
            ("ctrl+shift+up", "focus_up"),
            ("ctrl+shift+right", "focus_right"),
            ("ctrl+tab", "next_tab"),
            ("ctrl+shift+tab", "prev_tab"),
            ("ctrl+shift+c", "copy"),
            ("ctrl+shift+v", "paste"),
            ("ctrl+shift+plus", "increase_font_size"),
            ("ctrl+shift+minus", "decrease_font_size"),
            ("ctrl+shift+0", "reset_font_size"),
            ("ctrl+shift+r", "reload_config"),
            ("shift+pageup", "scroll_page_up"),
            ("shift+pagedown", "scroll_page_down"),
        ]
    }
}
