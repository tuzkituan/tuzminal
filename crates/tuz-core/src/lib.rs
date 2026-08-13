//! Terminal state for Tuzminal: PTY sessions, VT parsing, color resolution and
//! render snapshots.
//!
//! # Why this wraps `alacritty_terminal`
//!
//! Writing a correct VT implementation is months of work; `alacritty_terminal`
//! already ships one, along with a cross-platform PTY layer (`openpty` on unix,
//! ConPTY on Windows) and the I/O thread that pumps it. This crate uses all three
//! and confines them behind its own types, so the rest of Tuzminal depends on
//! [`RenderCell`] and [`TerminalFrame`] rather than on a dependency whose API is
//! semi-internal and does churn between releases.
//!
//! # Threading
//!
//! [`Session::spawn`] starts a background thread that owns the PTY and holds the
//! terminal behind a `FairMutex`. The UI thread takes that lock only to
//! [`snapshot`] the visible grid into owned data, then releases it. Fairness is
//! the important part: a plain mutex lets the PTY thread starve the UI under
//! heavy output, which is exactly when the user wants to press Ctrl-C.

pub mod color;
pub mod encode;
pub mod frame;
pub mod session;

pub use color::{resolve as resolve_colors, CellColors};
pub use encode::{encode, encode_paste};
pub use frame::{snapshot, CellFlags, RenderCell, RenderCursor, TerminalFrame, Underline};
pub use session::{event_name, EventProxy, PaneEvent, Session, SessionError, TermSize};

/// Re-exported so callers can match on terminal events and modes without taking
/// their own dependency on `alacritty_terminal`.
pub use alacritty_terminal::event::Event as TermEvent;
pub use alacritty_terminal::term::ClipboardType;
pub use alacritty_terminal::term::TermMode;

/// Mouse reporting state, for deciding whether a click belongs to the program or
/// to the terminal's own selection.
///
/// Programs like `htop` and `vim` enable mouse reporting; when they have, a drag
/// is theirs to interpret and starting a selection instead would break them.
/// Holding shift always forces terminal-side selection, which is the escape hatch
/// every terminal implements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReporting {
    pub click: bool,
    pub drag: bool,
    pub motion: bool,
    pub sgr: bool,
}

impl MouseReporting {
    pub fn from_mode(mode: TermMode) -> Self {
        Self {
            click: mode.contains(TermMode::MOUSE_REPORT_CLICK),
            drag: mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
        }
    }

    /// Whether the program wants mouse events at all.
    pub fn wants_mouse(&self) -> bool {
        self.click || self.drag || self.motion
    }
}

/// Mouse buttons the terminal can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl MouseButton {
    /// The button number in the X10/SGR encoding.
    fn code(self) -> u8 {
        match self {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            // Wheel events are reported as buttons 64 and 65.
            MouseButton::WheelUp => 64,
            MouseButton::WheelDown => 65,
        }
    }
}

/// Encode a mouse event for a program that requested mouse reporting.
///
/// Only the SGR form (`CSI < b ; x ; y M`) is emitted. The legacy X10 encoding
/// cannot express coordinates beyond column 223, which any modern window exceeds,
/// so a terminal that only speaks X10 silently misreports clicks on wide windows.
/// Returns `None` when the program has not enabled SGR reporting.
pub fn encode_mouse(
    button: MouseButton,
    pressed: bool,
    col: u16,
    row: u16,
    mods: tuz_input::Modifiers,
    reporting: MouseReporting,
) -> Option<Vec<u8>> {
    if !reporting.sgr {
        return None;
    }

    let mut code = button.code() as u32;
    // Modifier bits, as defined for mouse reports: shift=4, alt=8, ctrl=16.
    if mods.shift() {
        code += 4;
    }
    if mods.alt() {
        code += 8;
    }
    if mods.ctrl() {
        code += 16;
    }

    // Coordinates are 1-based on the wire.
    let x = col as u32 + 1;
    let y = row as u32 + 1;
    // Wheel events have no release; sending one confuses programs that toggle
    // state on button-up.
    let final_byte = if pressed || matches!(button, MouseButton::WheelUp | MouseButton::WheelDown) {
        'M'
    } else {
        'm'
    };

    Some(format!("\x1b[<{code};{x};{y}{final_byte}").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuz_input::Modifiers;

    fn sgr() -> MouseReporting {
        MouseReporting {
            click: true,
            drag: false,
            motion: false,
            sgr: true,
        }
    }

    #[test]
    fn mouse_reporting_reads_the_term_modes() {
        let r = MouseReporting::from_mode(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert!(r.click && r.sgr);
        assert!(!r.drag && !r.motion);
        assert!(r.wants_mouse());

        let off = MouseReporting::from_mode(TermMode::empty());
        assert!(!off.wants_mouse());
    }

    #[test]
    fn a_click_is_encoded_with_one_based_coordinates() {
        let out = encode_mouse(MouseButton::Left, true, 0, 0, Modifiers::NONE, sgr()).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<0;1;1M");
    }

    #[test]
    fn a_release_uses_the_lowercase_final_byte() {
        let out = encode_mouse(MouseButton::Left, false, 4, 9, Modifiers::NONE, sgr()).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<0;5;10m");
    }

    #[test]
    fn buttons_use_their_protocol_numbers() {
        let enc = |b| {
            String::from_utf8_lossy(&encode_mouse(b, true, 0, 0, Modifiers::NONE, sgr()).unwrap())
                .into_owned()
        };
        assert!(enc(MouseButton::Left).starts_with("\x1b[<0;"));
        assert!(enc(MouseButton::Middle).starts_with("\x1b[<1;"));
        assert!(enc(MouseButton::Right).starts_with("\x1b[<2;"));
        assert!(enc(MouseButton::WheelUp).starts_with("\x1b[<64;"));
        assert!(enc(MouseButton::WheelDown).starts_with("\x1b[<65;"));
    }

    #[test]
    fn modifiers_add_their_bits_to_the_button_code() {
        let out = encode_mouse(
            MouseButton::Left,
            true,
            0,
            0,
            Modifiers::CTRL | Modifiers::SHIFT,
            sgr(),
        )
        .unwrap();
        // 0 + shift(4) + ctrl(16) = 20
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<20;1;1M");
    }

    #[test]
    fn wheel_events_never_report_a_release() {
        // A spurious release confuses programs that toggle state on button-up.
        let out = encode_mouse(MouseButton::WheelUp, false, 0, 0, Modifiers::NONE, sgr()).unwrap();
        assert!(String::from_utf8_lossy(&out).ends_with('M'));
    }

    #[test]
    fn coordinates_beyond_the_x10_limit_still_encode_correctly() {
        // The reason only SGR is supported: X10 caps out at column 223.
        let out = encode_mouse(MouseButton::Left, true, 400, 300, Modifiers::NONE, sgr()).unwrap();
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[<0;401;301M");
    }

    #[test]
    fn nothing_is_encoded_when_the_program_did_not_ask_for_sgr() {
        let no_sgr = MouseReporting {
            click: true,
            drag: false,
            motion: false,
            sgr: false,
        };
        assert!(encode_mouse(MouseButton::Left, true, 0, 0, Modifiers::NONE, no_sgr).is_none());
    }
}
