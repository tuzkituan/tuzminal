//! Translation from winit key events to `tuz-input` chords.
//!
//! Isolated in its own module so the mapping can be unit-tested without a window
//! or an event loop. Keeping it separate also confines winit types to the shell
//! of the application rather than letting them reach the keymap.

use tuz_input::{Key, KeyChord, Modifiers, NamedKey};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey as WNamed};

/// Convert winit's modifier state into our bitset.
pub fn modifiers_from(state: ModifiersState) -> Modifiers {
    Modifiers::from_parts(
        state.control_key(),
        state.shift_key(),
        state.alt_key(),
        state.super_key(),
    )
}

/// Convert a winit logical key into a chord key.
///
/// Returns `None` for keys that cannot participate in a binding — dead keys,
/// bare modifier presses, and anything reported as an unidentified scancode.
pub fn key_from(logical: &WKey) -> Option<Key> {
    match logical {
        WKey::Character(s) => {
            // Multi-character input comes from an IME composition, which is text
            // to insert rather than a chord to match.
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(Key::Char(c)),
                _ => None,
            }
        }
        WKey::Named(named) => named_from(*named).map(Key::Named),
        // Dead keys and unidentified keys carry no binding-relevant identity.
        WKey::Dead(_) | WKey::Unidentified(_) => None,
    }
}

fn named_from(named: WNamed) -> Option<NamedKey> {
    Some(match named {
        WNamed::Enter => NamedKey::Enter,
        WNamed::Tab => NamedKey::Tab,
        WNamed::Backspace => NamedKey::Backspace,
        WNamed::Escape => NamedKey::Escape,
        WNamed::Space => NamedKey::Space,
        WNamed::Insert => NamedKey::Insert,
        WNamed::Delete => NamedKey::Delete,
        WNamed::Home => NamedKey::Home,
        WNamed::End => NamedKey::End,
        WNamed::PageUp => NamedKey::PageUp,
        WNamed::PageDown => NamedKey::PageDown,
        WNamed::ArrowLeft => NamedKey::Left,
        WNamed::ArrowRight => NamedKey::Right,
        WNamed::ArrowUp => NamedKey::Up,
        WNamed::ArrowDown => NamedKey::Down,
        WNamed::ContextMenu => NamedKey::Menu,
        WNamed::PrintScreen => NamedKey::PrintScreen,
        WNamed::Pause => NamedKey::Pause,
        WNamed::ScrollLock => NamedKey::ScrollLock,
        WNamed::NumLock => NamedKey::NumLock,
        WNamed::CapsLock => NamedKey::CapsLock,

        WNamed::F1 => NamedKey::Function(1),
        WNamed::F2 => NamedKey::Function(2),
        WNamed::F3 => NamedKey::Function(3),
        WNamed::F4 => NamedKey::Function(4),
        WNamed::F5 => NamedKey::Function(5),
        WNamed::F6 => NamedKey::Function(6),
        WNamed::F7 => NamedKey::Function(7),
        WNamed::F8 => NamedKey::Function(8),
        WNamed::F9 => NamedKey::Function(9),
        WNamed::F10 => NamedKey::Function(10),
        WNamed::F11 => NamedKey::Function(11),
        WNamed::F12 => NamedKey::Function(12),
        WNamed::F13 => NamedKey::Function(13),
        WNamed::F14 => NamedKey::Function(14),
        WNamed::F15 => NamedKey::Function(15),
        WNamed::F16 => NamedKey::Function(16),
        WNamed::F17 => NamedKey::Function(17),
        WNamed::F18 => NamedKey::Function(18),
        WNamed::F19 => NamedKey::Function(19),
        WNamed::F20 => NamedKey::Function(20),
        WNamed::F21 => NamedKey::Function(21),
        WNamed::F22 => NamedKey::Function(22),
        WNamed::F23 => NamedKey::Function(23),
        WNamed::F24 => NamedKey::Function(24),

        // Bare modifier presses are state changes, not chords.
        WNamed::Control | WNamed::Shift | WNamed::Alt | WNamed::Super | WNamed::Meta => {
            return None
        }
        _ => return None,
    })
}

/// Build a normalized chord from a key event.
///
/// Normalization matters here: the platform reports the *shifted* character, so
/// without it a binding written `ctrl+shift+d` would never match the `D` that
/// actually arrives.
pub fn chord_from(logical: &WKey, mods: ModifiersState) -> Option<KeyChord> {
    let key = key_from(logical)?;
    Some(KeyChord::new(modifiers_from(mods), key).normalized())
}

/// Decide what bytes a key press should send to the PTY.
///
/// Split out from the event handler so the rule can be tested directly: this is
/// where "I cannot type capital letters" lived. The chord used for *keymap lookup*
/// is normalized — `D` becomes `shift` plus `d` so `ctrl+shift+d` can be matched —
/// and encoding from that normalized chord silently lowercased every character the
/// user typed.
///
/// The rule:
///
/// - No ctrl/alt/super: use the platform's composed `text`, which already accounts
///   for keyboard layout and dead keys (`´` then `e` gives `é`). Control characters
///   are excluded so Enter and Tab still get their proper escape sequences.
/// - Otherwise: encode from the **raw** logical key, never the normalized one.
pub fn bytes_for_key(
    logical: &WKey,
    text: Option<&str>,
    mods: Modifiers,
    mode: tuz_core::TermMode,
) -> Option<Vec<u8>> {
    if !mods.ctrl() && !mods.alt() && !mods.super_key() {
        if let Some(text) = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) {
            return Some(text.as_bytes().to_vec());
        }
    }
    let key = key_from(logical)?;
    tuz_core::encode(key, mods, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// winit's `Character` payload is a `SmolStr`.
    fn character(s: &str) -> WKey {
        WKey::Character(s.into())
    }

    #[test]
    fn maps_plain_characters() {
        assert_eq!(key_from(&character("a")), Some(Key::Char('a')));
    }

    #[test]
    fn maps_named_keys() {
        assert_eq!(
            key_from(&WKey::Named(WNamed::Escape)),
            Some(Key::Named(NamedKey::Escape))
        );
        assert_eq!(
            key_from(&WKey::Named(WNamed::ArrowLeft)),
            Some(Key::Named(NamedKey::Left)),
            "winit's ArrowLeft is our Left"
        );
        assert_eq!(
            key_from(&WKey::Named(WNamed::F7)),
            Some(Key::Named(NamedKey::Function(7)))
        );
    }

    #[test]
    fn bare_modifier_presses_are_not_chords() {
        // Holding ctrl must not be treated as a binding attempt.
        for m in [WNamed::Control, WNamed::Shift, WNamed::Alt, WNamed::Super] {
            assert_eq!(key_from(&WKey::Named(m)), None, "{m:?} should be ignored");
        }
    }

    #[test]
    fn ime_composition_output_is_not_a_chord() {
        // A multi-character payload is text to insert, not a key to bind.
        assert_eq!(key_from(&character("ab")), None);
        assert_eq!(key_from(&character("")), None);
    }

    #[test]
    fn dead_keys_are_ignored() {
        assert_eq!(key_from(&WKey::Dead(Some('^'))), None);
    }

    #[test]
    fn modifier_state_translates_faithfully() {
        let m = modifiers_from(ModifiersState::CONTROL | ModifiersState::SHIFT);
        assert!(m.ctrl() && m.shift());
        assert!(!m.alt() && !m.super_key());
        assert_eq!(modifiers_from(ModifiersState::empty()), Modifiers::NONE);
    }

    #[test]
    fn a_shifted_character_event_matches_its_written_binding() {
        // The regression this guards: pressing ctrl+shift+d produces 'D', and a
        // chord built without normalizing would never equal `ctrl+shift+d`.
        let chord = chord_from(&character("D"), ModifiersState::CONTROL).unwrap();
        assert_eq!(chord, KeyChord::parse("ctrl+shift+d").unwrap());
    }

    #[test]
    fn a_space_key_event_matches_a_space_binding() {
        let chord = chord_from(&WKey::Named(WNamed::Space), ModifiersState::CONTROL).unwrap();
        assert_eq!(chord, KeyChord::parse("ctrl+space").unwrap());
    }

    #[test]
    fn unmapped_keys_produce_no_chord() {
        assert_eq!(chord_from(&WKey::Dead(None), ModifiersState::empty()), None);
    }
}

#[cfg(test)]
mod pty_bytes_tests {
    use super::*;
    use tuz_core::TermMode;

    fn character(s: &str) -> WKey {
        WKey::Character(s.into())
    }

    fn bytes(logical: &WKey, text: Option<&str>, mods: Modifiers) -> Option<Vec<u8>> {
        bytes_for_key(logical, text, mods, TermMode::empty())
    }

    #[test]
    fn a_capital_letter_reaches_the_pty_as_a_capital() {
        // The reported bug: uppercase was impossible to type. Shift+D arrives as
        // logical key `D` with text `"D"`, and must send `D`.
        let out = bytes(&character("D"), Some("D"), Modifiers::SHIFT).unwrap();
        assert_eq!(out, b"D");
    }

    #[test]
    fn every_capital_letter_survives() {
        for c in 'A'..='Z' {
            let s = c.to_string();
            let out = bytes(&character(&s), Some(&s), Modifiers::SHIFT)
                .unwrap_or_else(|| panic!("{c} produced nothing"));
            assert_eq!(out, s.as_bytes(), "for {c}");
        }
    }

    #[test]
    fn lowercase_still_works() {
        let out = bytes(&character("d"), Some("d"), Modifiers::NONE).unwrap();
        assert_eq!(out, b"d");
    }

    #[test]
    fn shifted_symbols_reach_the_pty() {
        for (key, expected) in [("!", "!"), ("@", "@"), ("~", "~"), ("?", "?")] {
            let out = bytes(&character(key), Some(key), Modifiers::SHIFT).unwrap();
            assert_eq!(out, expected.as_bytes(), "for {key}");
        }
    }

    #[test]
    fn ctrl_combinations_still_map_to_control_bytes() {
        // Ctrl must bypass the text path entirely, or ctrl+c sends "c" and nothing
        // can ever be interrupted.
        let out = bytes(&character("c"), Some("c"), Modifiers::CTRL).unwrap();
        assert_eq!(out, vec![0x03]);

        // Case-insensitively, so ctrl+shift+c is also 0x03.
        let out = bytes(
            &character("C"),
            Some("C"),
            Modifiers::CTRL | Modifiers::SHIFT,
        )
        .unwrap();
        assert_eq!(out, vec![0x03]);
    }

    #[test]
    fn alt_prefixes_with_escape_rather_than_sending_bare_text() {
        let out = bytes(&character("b"), Some("b"), Modifiers::ALT).unwrap();
        assert_eq!(out, vec![0x1b, b'b']);
    }

    #[test]
    fn control_characters_in_text_fall_through_to_the_encoder() {
        // winit reports "\r" as the text for Enter. Sending it verbatim happens to
        // be right, but Tab and Escape need the encoder's rules, so anything
        // control-shaped must not take the text shortcut.
        let out = bytes(&WKey::Named(WNamed::Enter), Some("\r"), Modifiers::NONE).unwrap();
        assert_eq!(out, b"\r");

        let out = bytes(&WKey::Named(WNamed::Tab), Some("\t"), Modifiers::NONE).unwrap();
        assert_eq!(out, b"\t");

        // Shift+Tab is back-tab, which only the encoder knows.
        let out = bytes(&WKey::Named(WNamed::Tab), Some("\t"), Modifiers::SHIFT).unwrap();
        assert_eq!(out, b"\x1b[Z");
    }

    #[test]
    fn a_space_is_sent_as_a_space() {
        let out = bytes(&WKey::Named(WNamed::Space), Some(" "), Modifiers::NONE).unwrap();
        assert_eq!(out, b" ");
    }

    #[test]
    fn multi_character_text_from_a_dead_key_is_sent_whole() {
        // `key_from` rejects multi-character input, so without the text path an
        // accented character composed from a dead key would be dropped entirely.
        let out = bytes(&character("é"), Some("é"), Modifiers::NONE).unwrap();
        assert_eq!(out, "é".as_bytes());

        let out = bytes(&WKey::Dead(Some('´')), Some("é"), Modifiers::NONE).unwrap();
        assert_eq!(out, "é".as_bytes(), "a dead-key composition must survive");
    }

    #[test]
    fn arrows_use_the_encoder_and_respect_application_mode() {
        // Arrows report no text, so they always go through the encoder.
        let out = bytes(&WKey::Named(WNamed::ArrowUp), None, Modifiers::NONE).unwrap();
        assert_eq!(out, b"\x1b[A");

        let out = bytes_for_key(
            &WKey::Named(WNamed::ArrowUp),
            None,
            Modifiers::NONE,
            TermMode::APP_CURSOR,
        )
        .unwrap();
        assert_eq!(out, b"\x1bOA");
    }

    #[test]
    fn missing_text_falls_back_to_the_key_itself() {
        // Some platforms report no text for an ordinary character.
        let out = bytes(&character("x"), None, Modifiers::NONE).unwrap();
        assert_eq!(out, b"x");
    }

    #[test]
    fn a_bare_modifier_press_sends_nothing() {
        assert_eq!(
            bytes(&WKey::Named(WNamed::Control), None, Modifiers::NONE),
            None
        );
        assert_eq!(
            bytes(&WKey::Named(WNamed::Shift), None, Modifiers::SHIFT),
            None
        );
    }

    #[test]
    fn empty_text_is_not_sent_as_an_empty_write() {
        // An empty write would be harmless but pointless; falling through to the
        // encoder is the correct handling.
        assert_eq!(bytes(&WKey::Dead(None), Some(""), Modifiers::NONE), None);
    }
}
