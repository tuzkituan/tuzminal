//! Encoding key presses into the byte sequences programs expect.
//!
//! This is the layer where "the arrow keys don't work in vim" bugs live. Three
//! terminal modes change the output for the same physical key:
//!
//! - **`APP_CURSOR`** (DECCKM) switches arrows from `CSI A` to `SS3 A`. `vim` and
//!   `less` enable it, and getting it wrong makes arrows insert `[A` instead of
//!   moving.
//! - **`APP_KEYPAD`** (DECKPAM) does the same for the numeric keypad.
//! - **modifiers** turn `CSI A` into `CSI 1;5A`, using a 1-based bitfield that is
//!   *not* the same encoding as anything else in the protocol.
//!
//! Reference: `xterm`'s `ctlseqs` document, which every program is written
//! against, quirks included.

use alacritty_terminal::term::TermMode;
use tuz_input::{Key, Modifiers, NamedKey};

/// The xterm modifier parameter: a 1-based bitfield.
///
/// shift=1, alt=2, ctrl=4, super=8, all plus one. Returns `None` when no
/// modifiers are held, which means the parameter must be omitted entirely rather
/// than sent as `1` — some programs mis-parse the explicit form.
fn modifier_param(mods: Modifiers) -> Option<u8> {
    let mut bits = 0u8;
    if mods.shift() {
        bits |= 1;
    }
    if mods.alt() {
        bits |= 2;
    }
    if mods.ctrl() {
        bits |= 4;
    }
    if mods.super_key() {
        bits |= 8;
    }
    (bits != 0).then_some(bits + 1)
}

/// Encode a key press for the PTY.
///
/// Returns `None` for keys that produce no output, so the caller can leave them
/// to other handling.
pub fn encode(key: Key, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    match key {
        Key::Named(named) => encode_named(named, mods, mode),
        Key::Char(c) => encode_char(c, mods),
    }
}

fn encode_char(c: char, mods: Modifiers) -> Option<Vec<u8>> {
    // Ctrl maps letters into the C0 control range: ctrl+a is 0x01. This is what
    // makes ctrl+c send SIGINT, so it must come before plain text encoding.
    if mods.ctrl() {
        if let Some(byte) = control_byte(c) {
            let mut out = Vec::with_capacity(2);
            // Alt is reported as an ESC prefix ("meta sends escape"), the default
            // every shell's readline configuration assumes.
            if mods.alt() {
                out.push(0x1b);
            }
            out.push(byte);
            return Some(out);
        }
        // Ctrl with a key that has no control mapping (ctrl+1) produces nothing,
        // matching xterm.
        if !mods.alt() {
            return None;
        }
    }

    let mut out = Vec::with_capacity(5);
    if mods.alt() {
        out.push(0x1b);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    Some(out)
}

/// The C0 control byte for a character, if it has one.
fn control_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        // The traditional aliases: ctrl+space and ctrl+@ are both NUL, and the
        // punctuation run maps onto 0x1b..0x1f.
        ' ' | '@' => 0x00,
        '[' => 0x1b,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        '?' => 0x7f,
        _ => return None,
    })
}

fn encode_named(named: NamedKey, mods: Modifiers, mode: TermMode) -> Option<Vec<u8>> {
    use NamedKey::*;

    // Keys with a fixed single-byte encoding, before any CSI handling.
    let simple: Option<&[u8]> = match named {
        Enter => Some(b"\r"),
        Tab if mods.shift() => Some(b"\x1b[Z"),
        Tab => Some(b"\t"),
        // DEL, not BS: this is what `stty erase` defaults to on Linux and macOS,
        // and sending 0x08 makes backspace print `^H` in most shells.
        Backspace if mods.ctrl() => Some(b"\x08"),
        Backspace => Some(b"\x7f"),
        Escape => Some(b"\x1b"),
        Space => Some(b" "),
        _ => None,
    };
    if let Some(bytes) = simple {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if mods.alt() && named != Tab {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        return Some(out);
    }

    // Cursor and edit keys. Two families:
    //   - "final byte" keys: CSI <mods> A  (or SS3 A in application mode)
    //   - "tilde" keys:      CSI <n> ; <mods> ~
    let param = modifier_param(mods);

    let final_byte = match named {
        Up => Some(b'A'),
        Down => Some(b'B'),
        Right => Some(b'C'),
        Left => Some(b'D'),
        End => Some(b'F'),
        Home => Some(b'H'),
        _ => None,
    };

    if let Some(byte) = final_byte {
        return Some(match param {
            // Modified keys always use the CSI form with an explicit `1;<mods>`,
            // even in application cursor mode — SS3 has no room for parameters.
            Some(p) => format!("\x1b[1;{p}{}", byte as char).into_bytes(),
            None if mode.contains(TermMode::APP_CURSOR) => {
                vec![0x1b, b'O', byte]
            }
            None => vec![0x1b, b'[', byte],
        });
    }

    let tilde = match named {
        Insert => Some(2),
        Delete => Some(3),
        PageUp => Some(5),
        PageDown => Some(6),
        Function(n @ 1..=4) => {
            // F1-F4 are SS3 P/Q/R/S when unmodified, and CSI 1;<mods>P when not.
            // This asymmetry is xterm's, and programs rely on it.
            let byte = b'P' + (n - 1);
            return Some(match param {
                Some(p) => format!("\x1b[1;{p}{}", byte as char).into_bytes(),
                None => vec![0x1b, b'O', byte],
            });
        }
        // The F5-F12 numbering skips values, another xterm quirk.
        Function(5) => Some(15),
        Function(6) => Some(17),
        Function(7) => Some(18),
        Function(8) => Some(19),
        Function(9) => Some(20),
        Function(10) => Some(21),
        Function(11) => Some(23),
        Function(12) => Some(24),
        Function(13) => Some(25),
        Function(14) => Some(26),
        Function(15) => Some(28),
        Function(16) => Some(29),
        Function(17) => Some(31),
        Function(18) => Some(32),
        Function(19) => Some(33),
        Function(20) => Some(34),
        _ => None,
    };

    if let Some(n) = tilde {
        return Some(match param {
            Some(p) => format!("\x1b[{n};{p}~").into_bytes(),
            None => format!("\x1b[{n}~").into_bytes(),
        });
    }

    // Lock keys, Pause, PrintScreen and F21+ produce nothing.
    None
}

/// Encode text for a bracketed paste.
///
/// When the program has enabled bracketed paste it needs the wrapper so it can
/// tell pasted text from typed text — without it, pasting into `vim` in insert
/// mode triggers autoindent on every line and mangles the content.
///
/// The paste content is also sanitized: an embedded `ESC [ 2 0 1 ~` would end the
/// paste early and let the rest be interpreted as keystrokes, which is a real
/// injection vector when pasting untrusted text.
pub fn encode_paste(text: &str, mode: TermMode) -> Vec<u8> {
    if !mode.contains(TermMode::BRACKETED_PASTE) {
        // Outside bracketed paste, carriage returns are what a program expects
        // from the Enter key; leaving \n would submit nothing in many shells.
        return text.replace("\r\n", "\r").replace('\n', "\r").into_bytes();
    }

    let sanitized = text.replace("\x1b[201~", "");
    let mut out = Vec::with_capacity(sanitized.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(
        sanitized
            .replace("\r\n", "\r")
            .replace('\n', "\r")
            .as_bytes(),
    );
    out.extend_from_slice(b"\x1b[201~");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;

    fn enc(key: Key, mods: Modifiers, mode: TermMode) -> String {
        String::from_utf8_lossy(&encode(key, mods, mode).expect("should encode")).into_owned()
    }

    fn named(k: NamedKey, mods: Modifiers, mode: TermMode) -> String {
        enc(Key::Named(k), mods, mode)
    }

    fn ch(c: char, mods: Modifiers) -> String {
        enc(Key::Char(c), mods, TermMode::empty())
    }

    #[test]
    fn plain_characters_pass_through_as_utf8() {
        assert_eq!(ch('a', NONE), "a");
        assert_eq!(ch('Z', NONE), "Z");
        assert_eq!(ch('é', NONE), "é");
        assert_eq!(ch('日', NONE), "日");
    }

    #[test]
    fn ctrl_letters_map_to_c0_controls() {
        // ctrl+c must be 0x03 or SIGINT never fires.
        assert_eq!(
            encode(Key::Char('c'), Modifiers::CTRL, TermMode::empty()),
            Some(vec![0x03])
        );
        assert_eq!(
            encode(Key::Char('a'), Modifiers::CTRL, TermMode::empty()),
            Some(vec![0x01])
        );
        assert_eq!(
            encode(Key::Char('z'), Modifiers::CTRL, TermMode::empty()),
            Some(vec![0x1a])
        );
        // Case must not matter: ctrl+shift+C is still 0x03.
        assert_eq!(
            encode(Key::Char('C'), Modifiers::CTRL, TermMode::empty()),
            Some(vec![0x03])
        );
    }

    #[test]
    fn ctrl_punctuation_follows_the_traditional_mapping() {
        for (c, byte) in [
            (' ', 0x00u8),
            ('@', 0x00),
            ('[', 0x1b),
            ('\\', 0x1c),
            (']', 0x1d),
            ('^', 0x1e),
            ('_', 0x1f),
            ('?', 0x7f),
        ] {
            assert_eq!(
                encode(Key::Char(c), Modifiers::CTRL, TermMode::empty()),
                Some(vec![byte]),
                "ctrl+{c}"
            );
        }
    }

    #[test]
    fn ctrl_with_an_unmapped_key_produces_nothing() {
        // xterm sends nothing for ctrl+1; inserting a literal "1" would be wrong.
        assert_eq!(
            encode(Key::Char('1'), Modifiers::CTRL, TermMode::empty()),
            None
        );
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(ch('a', Modifiers::ALT), "\x1ba");
        // alt+ctrl combines both: ESC then the control byte.
        assert_eq!(
            encode(
                Key::Char('c'),
                Modifiers::CTRL | Modifiers::ALT,
                TermMode::empty()
            ),
            Some(vec![0x1b, 0x03])
        );
    }

    #[test]
    fn backspace_sends_del_not_backspace() {
        // Sending 0x08 here makes backspace print ^H in most shells.
        assert_eq!(named(NamedKey::Backspace, NONE, TermMode::empty()), "\x7f");
        assert_eq!(
            named(NamedKey::Backspace, Modifiers::CTRL, TermMode::empty()),
            "\x08"
        );
    }

    #[test]
    fn enter_and_tab_and_escape() {
        assert_eq!(named(NamedKey::Enter, NONE, TermMode::empty()), "\r");
        assert_eq!(named(NamedKey::Tab, NONE, TermMode::empty()), "\t");
        assert_eq!(named(NamedKey::Escape, NONE, TermMode::empty()), "\x1b");
        // Shift+Tab is back-tab, which shells use for reverse completion.
        assert_eq!(
            named(NamedKey::Tab, Modifiers::SHIFT, TermMode::empty()),
            "\x1b[Z"
        );
    }

    #[test]
    fn arrows_use_csi_in_normal_mode() {
        let m = TermMode::empty();
        assert_eq!(named(NamedKey::Up, NONE, m), "\x1b[A");
        assert_eq!(named(NamedKey::Down, NONE, m), "\x1b[B");
        assert_eq!(named(NamedKey::Right, NONE, m), "\x1b[C");
        assert_eq!(named(NamedKey::Left, NONE, m), "\x1b[D");
    }

    #[test]
    fn arrows_use_ss3_in_application_cursor_mode() {
        // This is the "arrows insert [A in vim" bug. vim sets APP_CURSOR.
        let m = TermMode::APP_CURSOR;
        assert_eq!(named(NamedKey::Up, NONE, m), "\x1bOA");
        assert_eq!(named(NamedKey::Left, NONE, m), "\x1bOD");
    }

    #[test]
    fn modified_arrows_use_csi_even_in_application_mode() {
        // SS3 cannot carry parameters, so a modified key must fall back to CSI
        // regardless of mode.
        assert_eq!(
            named(NamedKey::Up, Modifiers::CTRL, TermMode::APP_CURSOR),
            "\x1b[1;5A"
        );
    }

    #[test]
    fn the_modifier_parameter_is_the_xterm_bitfield() {
        let m = TermMode::empty();
        // shift=1, alt=2, ctrl=4, super=8, each plus one.
        assert_eq!(named(NamedKey::Up, Modifiers::SHIFT, m), "\x1b[1;2A");
        assert_eq!(named(NamedKey::Up, Modifiers::ALT, m), "\x1b[1;3A");
        assert_eq!(named(NamedKey::Up, Modifiers::CTRL, m), "\x1b[1;5A");
        assert_eq!(
            named(NamedKey::Up, Modifiers::CTRL | Modifiers::SHIFT, m),
            "\x1b[1;6A"
        );
        assert_eq!(
            named(
                NamedKey::Up,
                Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT,
                m
            ),
            "\x1b[1;8A"
        );
    }

    #[test]
    fn unmodified_keys_omit_the_parameter_entirely() {
        // Sending the explicit `1` form is mis-parsed by some programs.
        assert_eq!(modifier_param(NONE), None);
        assert!(!named(NamedKey::Up, NONE, TermMode::empty()).contains(';'));
    }

    #[test]
    fn home_and_end_use_their_letter_forms() {
        let m = TermMode::empty();
        assert_eq!(named(NamedKey::Home, NONE, m), "\x1b[H");
        assert_eq!(named(NamedKey::End, NONE, m), "\x1b[F");
    }

    #[test]
    fn edit_keys_use_the_tilde_form() {
        let m = TermMode::empty();
        assert_eq!(named(NamedKey::Insert, NONE, m), "\x1b[2~");
        assert_eq!(named(NamedKey::Delete, NONE, m), "\x1b[3~");
        assert_eq!(named(NamedKey::PageUp, NONE, m), "\x1b[5~");
        assert_eq!(named(NamedKey::PageDown, NONE, m), "\x1b[6~");
        // Modified: the parameter goes after the number.
        assert_eq!(named(NamedKey::Delete, Modifiers::CTRL, m), "\x1b[3;5~");
    }

    #[test]
    fn f1_to_f4_use_ss3_unmodified_and_csi_when_modified() {
        let m = TermMode::empty();
        assert_eq!(named(NamedKey::Function(1), NONE, m), "\x1bOP");
        assert_eq!(named(NamedKey::Function(4), NONE, m), "\x1bOS");
        assert_eq!(
            named(NamedKey::Function(1), Modifiers::SHIFT, m),
            "\x1b[1;2P"
        );
    }

    #[test]
    fn f5_onward_use_the_tilde_form_with_xterms_skipped_numbering() {
        let m = TermMode::empty();
        // The gaps (16, 22, 27, 30) are xterm's; programs are written against them.
        assert_eq!(named(NamedKey::Function(5), NONE, m), "\x1b[15~");
        assert_eq!(named(NamedKey::Function(6), NONE, m), "\x1b[17~");
        assert_eq!(named(NamedKey::Function(10), NONE, m), "\x1b[21~");
        assert_eq!(named(NamedKey::Function(11), NONE, m), "\x1b[23~");
        assert_eq!(named(NamedKey::Function(12), NONE, m), "\x1b[24~");
    }

    #[test]
    fn keys_with_no_encoding_return_none() {
        let m = TermMode::empty();
        for k in [
            NamedKey::CapsLock,
            NamedKey::NumLock,
            NamedKey::ScrollLock,
            NamedKey::Pause,
            NamedKey::PrintScreen,
            NamedKey::Function(22),
        ] {
            assert_eq!(encode(Key::Named(k), NONE, m), None, "{k:?}");
        }
    }

    // --- paste ------------------------------------------------------------

    #[test]
    fn paste_is_wrapped_when_the_program_asked_for_it() {
        let out = encode_paste("hello", TermMode::BRACKETED_PASTE);
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[200~hello\x1b[201~");
    }

    #[test]
    fn paste_is_bare_when_bracketed_paste_is_off() {
        let out = encode_paste("hello", TermMode::empty());
        assert_eq!(String::from_utf8_lossy(&out), "hello");
    }

    #[test]
    fn newlines_become_carriage_returns() {
        // A shell reading a line wants \r; \n alone submits nothing.
        let out = encode_paste("a\nb\r\nc", TermMode::empty());
        assert_eq!(String::from_utf8_lossy(&out), "a\rb\rc");
    }

    #[test]
    fn an_embedded_paste_terminator_is_stripped() {
        // Without this, pasting untrusted text could end the paste early and let
        // the remainder run as keystrokes.
        let malicious = "safe\x1b[201~rm -rf /\r";
        let out = encode_paste(malicious, TermMode::BRACKETED_PASTE);
        let s = String::from_utf8_lossy(&out);

        assert_eq!(
            s.matches("\x1b[201~").count(),
            1,
            "only the real terminator may remain: {s:?}"
        );
        assert!(s.starts_with("\x1b[200~"));
        assert!(s.ends_with("\x1b[201~"));
    }
}

#[cfg(test)]
mod capital_letter_regression {
    use super::*;
    use tuz_input::KeyChord;

    /// Capital letters must survive to the PTY.
    ///
    /// The bug this pins down: the app looked up keybindings using a *normalized*
    /// chord — which lowercases the character so `ctrl+shift+d` can be matched — and
    /// then encoded from that same normalized chord. Every capital letter arrived
    /// lowercase, making it impossible to type uppercase text at all.
    #[test]
    fn encoding_a_capital_letter_yields_that_capital() {
        let out = encode(Key::Char('D'), Modifiers::SHIFT, TermMode::empty()).unwrap();
        assert_eq!(out, b"D", "shift+d must send 'D', not 'd'");

        for c in ['A', 'Z', 'Q'] {
            let out = encode(Key::Char(c), Modifiers::SHIFT, TermMode::empty()).unwrap();
            assert_eq!(out, c.to_string().as_bytes(), "for {c}");
        }
    }

    #[test]
    fn a_normalized_chord_is_the_wrong_thing_to_encode() {
        // Demonstrates why the app must not encode from the chord it looks up with:
        // normalization is lossy in exactly the direction that breaks typing.
        let normalized = KeyChord::char(Modifiers::NONE, 'D').normalized();
        assert_eq!(normalized.key, Key::Char('d'), "normalization lowercases");

        let wrong = encode(normalized.key, normalized.mods, TermMode::empty()).unwrap();
        assert_eq!(
            wrong, b"d",
            "which is how the bug produced lowercase output"
        );
    }

    #[test]
    fn shift_does_not_disturb_control_combinations() {
        // ctrl+shift+d must still be 0x04: the control mapping is case-insensitive.
        let out = encode(
            Key::Char('D'),
            Modifiers::CTRL | Modifiers::SHIFT,
            TermMode::empty(),
        )
        .unwrap();
        assert_eq!(out, vec![0x04]);
    }

    #[test]
    fn shifted_symbols_pass_through_unchanged() {
        for c in ['!', '@', '#', '$', '~', '?', '{'] {
            let out = encode(Key::Char(c), Modifiers::SHIFT, TermMode::empty()).unwrap();
            assert_eq!(out, c.to_string().as_bytes(), "for {c}");
        }
    }
}
