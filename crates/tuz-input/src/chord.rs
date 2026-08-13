//! Keychord parsing.
//!
//! A chord is written `ctrl+shift+d`: zero or more modifiers, then exactly one
//! key, joined by `+`. Parsing is case-insensitive and accepts the common aliases
//! for each name, because a user who writes `esc`, `escape`, `pgup` or `PageUp`
//! means the same thing every time.
//!
//! # Shift and character keys
//!
//! Chords match on the key *before* shift is applied: `ctrl+shift+d`, never
//! `ctrl+D`. Windowing systems report the shifted character (`D`), so
//! [`KeyChord::normalized`] folds it back. Without that, a binding would depend
//! on keyboard layout in ways nobody can debug.

use std::fmt;

/// Modifier keys held during a key press, as a small bitset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const CTRL: Self = Self(1 << 0);
    pub const SHIFT: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    /// Command on macOS, Windows key elsewhere.
    pub const SUPER: Self = Self(1 << 3);

    pub const fn empty() -> Self {
        Self::NONE
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn ctrl(self) -> bool {
        self.contains(Self::CTRL)
    }
    pub const fn shift(self) -> bool {
        self.contains(Self::SHIFT)
    }
    pub const fn alt(self) -> bool {
        self.contains(Self::ALT)
    }
    pub const fn super_key(self) -> bool {
        self.contains(Self::SUPER)
    }

    pub fn from_parts(ctrl: bool, shift: bool, alt: bool, super_key: bool) -> Self {
        let mut m = Self::NONE;
        if ctrl {
            m = m.union(Self::CTRL);
        }
        if shift {
            m = m.union(Self::SHIFT);
        }
        if alt {
            m = m.union(Self::ALT);
        }
        if super_key {
            m = m.union(Self::SUPER);
        }
        m
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Fixed order so a chord always renders identically regardless of how
        // the user wrote it.
        for (flag, name) in [
            (Self::CTRL, "ctrl"),
            (Self::ALT, "alt"),
            (Self::SHIFT, "shift"),
            (Self::SUPER, "super"),
        ] {
            if self.contains(flag) {
                write!(f, "{name}+")?;
            }
        }
        Ok(())
    }
}

/// A key that is not a printable character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NamedKey {
    Enter,
    Tab,
    Backspace,
    Escape,
    Space,
    Insert,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    /// `F1`..`F24`.
    Function(u8),
    Menu,
    PrintScreen,
    Pause,
    ScrollLock,
    NumLock,
    CapsLock,
}

impl NamedKey {
    fn canonical_name(self) -> &'static str {
        match self {
            NamedKey::Enter => "enter",
            NamedKey::Tab => "tab",
            NamedKey::Backspace => "backspace",
            NamedKey::Escape => "escape",
            NamedKey::Space => "space",
            NamedKey::Insert => "insert",
            NamedKey::Delete => "delete",
            NamedKey::Home => "home",
            NamedKey::End => "end",
            NamedKey::PageUp => "pageup",
            NamedKey::PageDown => "pagedown",
            NamedKey::Left => "left",
            NamedKey::Right => "right",
            NamedKey::Up => "up",
            NamedKey::Down => "down",
            NamedKey::Function(_) => "f",
            NamedKey::Menu => "menu",
            NamedKey::PrintScreen => "printscreen",
            NamedKey::Pause => "pause",
            NamedKey::ScrollLock => "scrolllock",
            NamedKey::NumLock => "numlock",
            NamedKey::CapsLock => "capslock",
        }
    }

    /// Recognize a key name, accepting the usual aliases.
    fn parse(s: &str) -> Option<Self> {
        // Function keys are handled first so `f1` never collides with the
        // character `f`.
        if let Some(rest) = s.strip_prefix('f') {
            if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
                if let Ok(n) = rest.parse::<u8>() {
                    if (1..=24).contains(&n) {
                        return Some(NamedKey::Function(n));
                    }
                }
                // `f0` or `f99`: a clear attempt at a function key, so fall
                // through to None rather than matching something else.
                return None;
            }
        }

        Some(match s {
            "enter" | "return" | "cr" => NamedKey::Enter,
            "tab" => NamedKey::Tab,
            "backspace" | "bs" => NamedKey::Backspace,
            "escape" | "esc" => NamedKey::Escape,
            "space" => NamedKey::Space,
            "insert" | "ins" => NamedKey::Insert,
            "delete" | "del" => NamedKey::Delete,
            "home" => NamedKey::Home,
            "end" => NamedKey::End,
            "pageup" | "pgup" | "prior" => NamedKey::PageUp,
            "pagedown" | "pgdn" | "pgdown" | "next" => NamedKey::PageDown,
            "left" => NamedKey::Left,
            "right" => NamedKey::Right,
            "up" => NamedKey::Up,
            "down" => NamedKey::Down,
            "menu" => NamedKey::Menu,
            "printscreen" | "prtsc" => NamedKey::PrintScreen,
            "pause" => NamedKey::Pause,
            "scrolllock" => NamedKey::ScrollLock,
            "numlock" => NamedKey::NumLock,
            "capslock" => NamedKey::CapsLock,
            _ => return None,
        })
    }
}

impl fmt::Display for NamedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NamedKey::Function(n) => write!(f, "f{n}"),
            other => write!(f, "{}", other.canonical_name()),
        }
    }
}

/// The key half of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A printable character, always stored lowercase.
    Char(char),
    Named(NamedKey),
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char('+') => write!(f, "plus"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Named(k) => write!(f, "{k}"),
        }
    }
}

/// A modifier combination plus one key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub mods: Modifiers,
    pub key: Key,
}

impl KeyChord {
    pub const fn new(mods: Modifiers, key: Key) -> Self {
        Self { mods, key }
    }

    pub const fn char(mods: Modifiers, c: char) -> Self {
        Self::new(mods, Key::Char(c))
    }

    pub const fn named(mods: Modifiers, k: NamedKey) -> Self {
        Self::new(mods, Key::Named(k))
    }

    /// Parse a chord such as `ctrl+shift+d`.
    pub fn parse(s: &str) -> Result<Self, ChordParseError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ChordParseError::Empty);
        }
        let lower = trimmed.to_lowercase();

        // Splitting on '+' is ambiguous when '+' is itself the key. A trailing
        // empty segment means the last '+' was the key, e.g. `ctrl++`.
        let segments: Vec<&str> = lower.split('+').collect();
        let (key_str, mod_strs): (String, &[&str]) =
            if segments.len() >= 2 && segments[segments.len() - 1].is_empty() {
                ("+".to_owned(), &segments[..segments.len() - 2])
            } else {
                (
                    segments[segments.len() - 1].to_owned(),
                    &segments[..segments.len() - 1],
                )
            };

        let mut mods = Modifiers::NONE;
        for m in mod_strs {
            let flag = match *m {
                "ctrl" | "control" | "ctl" => Modifiers::CTRL,
                "shift" => Modifiers::SHIFT,
                "alt" | "option" | "opt" | "meta" => Modifiers::ALT,
                "super" | "cmd" | "command" | "win" | "windows" | "logo" => Modifiers::SUPER,
                "" => return Err(ChordParseError::EmptySegment(trimmed.to_owned())),
                other => {
                    return Err(ChordParseError::UnknownModifier {
                        chord: trimmed.to_owned(),
                        modifier: other.to_owned(),
                    })
                }
            };
            if mods.contains(flag) {
                return Err(ChordParseError::DuplicateModifier {
                    chord: trimmed.to_owned(),
                    modifier: (*m).to_owned(),
                });
            }
            mods = mods.union(flag);
        }

        let key = parse_key(&key_str).ok_or_else(|| ChordParseError::UnknownKey {
            chord: trimmed.to_owned(),
            key: key_str.clone(),
        })?;

        Ok(Self::new(mods, key).normalized())
    }

    /// Fold a shifted character back to its base form.
    ///
    /// Windowing systems report `D` for shift+d and `+` for shift+=, so a chord
    /// built from a live key event must be normalized before lookup or it will
    /// never match the parsed binding.
    pub fn normalized(self) -> Self {
        match self.key {
            Key::Char(c) if c.is_uppercase() => Self {
                // An uppercase character only arrives with shift held, so record
                // shift explicitly and store the lowercase key.
                mods: self.mods.union(Modifiers::SHIFT),
                key: Key::Char(c.to_lowercase().next().unwrap_or(c)),
            },
            // A literal space is the Space key; treating it as Char(' ') would
            // make `ctrl+space` fail to match a binding written as `ctrl+space`.
            Key::Char(' ') => Self {
                mods: self.mods,
                key: Key::Named(NamedKey::Space),
            },
            _ => self,
        }
    }
}

fn parse_key(s: &str) -> Option<Key> {
    if let Some(named) = NamedKey::parse(s) {
        return Some(Key::Named(named));
    }
    // Symbolic aliases for keys that cannot appear literally in a chord, or
    // that read badly when they do.
    match s {
        "plus" => return Some(Key::Char('+')),
        "minus" | "hyphen" | "dash" => return Some(Key::Char('-')),
        "equal" | "equals" => return Some(Key::Char('=')),
        "comma" => return Some(Key::Char(',')),
        "period" | "dot" => return Some(Key::Char('.')),
        "slash" => return Some(Key::Char('/')),
        "backslash" => return Some(Key::Char('\\')),
        "semicolon" => return Some(Key::Char(';')),
        "quote" | "apostrophe" => return Some(Key::Char('\'')),
        "backtick" | "grave" => return Some(Key::Char('`')),
        "backquote" => return Some(Key::Char('`')),
        "leftbracket" | "lbracket" => return Some(Key::Char('[')),
        "rightbracket" | "rbracket" => return Some(Key::Char(']')),
        _ => {}
    }

    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(Key::Char(c)),
        // More than one character and not a recognized name.
        _ => None,
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Routed through `pad` so width and alignment specifiers work. A plain
        // `write!` here silently ignores `{:<24}`, which quietly breaks any
        // column-aligned listing of keybindings.
        f.pad(&format!("{}{}", self.mods, self.key))
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ChordParseError {
    #[error("keychord is empty")]
    Empty,
    #[error("keychord `{0}` has an empty component")]
    EmptySegment(String),
    #[error(
        "keychord `{chord}`: unknown modifier `{modifier}` (expected ctrl, shift, alt or super)"
    )]
    UnknownModifier { chord: String, modifier: String },
    #[error("keychord `{chord}`: modifier `{modifier}` is repeated")]
    DuplicateModifier { chord: String, modifier: String },
    #[error("keychord `{chord}`: unknown key `{key}`")]
    UnknownKey { chord: String, key: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap_or_else(|e| panic!("`{s}` should parse: {e}"))
    }

    #[test]
    fn parses_a_plain_character() {
        assert_eq!(chord("a"), KeyChord::char(Modifiers::NONE, 'a'));
    }

    #[test]
    fn parses_modifiers_in_any_order() {
        let expected = KeyChord::char(Modifiers::CTRL | Modifiers::SHIFT, 'd');
        assert_eq!(chord("ctrl+shift+d"), expected);
        assert_eq!(chord("shift+ctrl+d"), expected, "order must not matter");
    }

    #[test]
    fn parsing_is_case_insensitive() {
        assert_eq!(chord("CTRL+Shift+D"), chord("ctrl+shift+d"));
        assert_eq!(chord("PageUp"), chord("pageup"));
    }

    #[test]
    fn accepts_modifier_aliases() {
        let m = chord("ctrl+a").mods;
        assert_eq!(chord("control+a").mods, m);
        assert_eq!(chord("ctl+a").mods, m);
        assert_eq!(chord("cmd+a").mods, Modifiers::SUPER);
        assert_eq!(chord("win+a").mods, Modifiers::SUPER);
        assert_eq!(chord("option+a").mods, Modifiers::ALT);
    }

    #[test]
    fn accepts_named_key_aliases() {
        assert_eq!(chord("esc").key, Key::Named(NamedKey::Escape));
        assert_eq!(chord("escape").key, Key::Named(NamedKey::Escape));
        assert_eq!(chord("return").key, Key::Named(NamedKey::Enter));
        assert_eq!(chord("pgup").key, Key::Named(NamedKey::PageUp));
        assert_eq!(chord("pgdn").key, Key::Named(NamedKey::PageDown));
        assert_eq!(chord("del").key, Key::Named(NamedKey::Delete));
    }

    #[test]
    fn parses_function_keys_without_colliding_with_the_letter_f() {
        assert_eq!(chord("f1").key, Key::Named(NamedKey::Function(1)));
        assert_eq!(chord("f24").key, Key::Named(NamedKey::Function(24)));
        // A bare `f` is still the character.
        assert_eq!(chord("f").key, Key::Char('f'));
        // Out-of-range function keys are an error, not a silent fallback.
        assert!(KeyChord::parse("f0").is_err());
        assert!(KeyChord::parse("f25").is_err());
    }

    #[test]
    fn handles_plus_as_a_key() {
        // The ambiguous case: `+` is both the separator and a key.
        assert_eq!(chord("ctrl++").key, Key::Char('+'));
        assert_eq!(chord("ctrl++").mods, Modifiers::CTRL);
        assert_eq!(chord("+").key, Key::Char('+'));
        // And the unambiguous spelling means the same thing.
        assert_eq!(chord("plus"), chord("+"));
        assert_eq!(chord("ctrl+shift+plus"), chord("ctrl+shift++"));
    }

    #[test]
    fn handles_symbolic_key_names() {
        assert_eq!(chord("minus").key, Key::Char('-'));
        assert_eq!(chord("comma").key, Key::Char(','));
        assert_eq!(chord("slash").key, Key::Char('/'));
        assert_eq!(chord("grave").key, Key::Char('`'));
        assert_eq!(chord("lbracket").key, Key::Char('['));
    }

    #[test]
    fn shifted_characters_normalize_to_base_key_plus_shift() {
        // The bug this prevents: a key event reporting 'D' never matching a
        // binding written as `ctrl+shift+d`.
        let from_event = KeyChord::char(Modifiers::CTRL, 'D').normalized();
        assert_eq!(from_event, chord("ctrl+shift+d"));
    }

    #[test]
    fn a_literal_space_becomes_the_space_key() {
        let from_event = KeyChord::char(Modifiers::CTRL, ' ').normalized();
        assert_eq!(from_event, chord("ctrl+space"));
    }

    #[test]
    fn rejects_malformed_chords() {
        assert_eq!(KeyChord::parse(""), Err(ChordParseError::Empty));
        assert_eq!(KeyChord::parse("   "), Err(ChordParseError::Empty));
        assert!(matches!(
            KeyChord::parse("hyper+a"),
            Err(ChordParseError::UnknownModifier { .. })
        ));
        assert!(matches!(
            KeyChord::parse("ctrl+ctrl+a"),
            Err(ChordParseError::DuplicateModifier { .. })
        ));
        assert!(matches!(
            KeyChord::parse("ctrl+notakey"),
            Err(ChordParseError::UnknownKey { .. })
        ));
        assert!(matches!(
            KeyChord::parse("ctrl++a"),
            Err(ChordParseError::EmptySegment(_))
        ));
    }

    #[test]
    fn error_messages_quote_the_offending_chord() {
        let e = KeyChord::parse("hyper+a").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("hyper+a"), "{msg}");
        assert!(msg.contains("hyper"), "{msg}");
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in [
            "a",
            "ctrl+shift+d",
            "alt+f4",
            "super+space",
            "ctrl+pageup",
            "shift+plus",
            "ctrl+alt+shift+super+x",
        ] {
            let c = chord(s);
            let printed = c.to_string();
            assert_eq!(
                chord(&printed),
                c,
                "`{s}` printed as `{printed}` did not round-trip"
            );
        }
    }

    #[test]
    fn display_honors_width_specifiers() {
        // Guards the `f.pad` in the Display impl: a plain `write!` would ignore
        // the width and quietly ruin any aligned keybinding listing.
        assert_eq!(format!("[{:<12}]", chord("ctrl+d")), "[ctrl+d      ]");
        assert_eq!(format!("[{:>12}]", chord("ctrl+d")), "[      ctrl+d]");
    }

    #[test]
    fn display_uses_a_canonical_modifier_order() {
        // Two spellings of the same chord must print identically, or a UI listing
        // keybinds would show duplicates.
        assert_eq!(chord("shift+ctrl+d").to_string(), "ctrl+shift+d");
        assert_eq!(chord("ctrl+shift+d").to_string(), "ctrl+shift+d");
    }

    #[test]
    fn modifier_set_operations() {
        let m = Modifiers::CTRL | Modifiers::SHIFT;
        assert!(m.ctrl() && m.shift());
        assert!(!m.alt() && !m.super_key());
        assert!(m.contains(Modifiers::CTRL));
        assert!(!m.contains(Modifiers::ALT));
        assert_eq!(m.without(Modifiers::SHIFT), Modifiers::CTRL);
        assert!(Modifiers::NONE.is_empty());
        assert_eq!(
            Modifiers::from_parts(true, false, true, false),
            Modifiers::CTRL | Modifiers::ALT
        );
    }
}
