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
