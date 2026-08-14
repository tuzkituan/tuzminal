//! Snapshotting terminal state into a render-ready form.
//!
//! The renderer must not hold the terminal mutex — the PTY thread needs it, and a
//! long lock during a frame shows up as stutter under heavy output. So a frame
//! begins by copying the visible grid into plain owned data and releasing the
//! lock immediately.
//!
//! This snapshot is also where `alacritty_terminal`'s types stop. Everything
//! downstream sees our own [`RenderCell`] and [`CellFlags`], which keeps the
//! renderer independent of that dependency's churn.

use crate::color::{self, CellColors};
use crate::session::EventProxy;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::CursorShape as AnsiCursorShape;
use tuz_config::{Config, CursorShape, Rgba, Theme};

/// Per-cell rendering attributes, independent of the VT library's own bitset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellFlags {
    pub bold: bool,
    pub italic: bool,
    pub strikeout: bool,
    pub underline: Underline,
    /// A double-width glyph (CJK, most emoji) occupying two columns.
    pub wide: bool,
}

/// Which underline style a cell carries, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// One cell, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct RenderCell {
    pub col: u16,
    /// Row within the viewport, 0 at the top.
    pub row: u16,
    pub ch: char,
    /// Combining marks that stack onto `ch`, e.g. an accent or a Devanagari
    /// matra. Rendered on top of the base glyph in the same cell.
    pub zerowidth: Vec<char>,
    pub fg: Rgba,
    pub bg: Rgba,
    /// Explicit underline color from SGR 58, which is independent of `fg`.
    pub underline_color: Option<Rgba>,
    pub flags: CellFlags,
}

impl RenderCell {
    /// True when the cell would draw nothing but its background.
    pub fn is_blank(&self) -> bool {
        (self.ch == ' ' || self.ch == '\0') && self.zerowidth.is_empty()
    }
}

/// Where and how to draw the cursor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderCursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub color: Rgba,
    /// Color for the glyph beneath a block cursor.
    pub text_color: Rgba,
    /// Thickness for beam and underline shapes, as a fraction of the cell.
    pub thickness: f32,
}

/// An immutable snapshot of one pane's visible content.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFrame {
    pub cells: Vec<RenderCell>,
    /// `None` when the cursor is hidden or scrolled out of view.
    pub cursor: Option<RenderCursor>,
    pub columns: u16,
    pub rows: u16,
    /// How far the viewport is scrolled back. Non-zero means history is showing,
    /// which the UI reflects by hiding the cursor.
    pub display_offset: usize,
    /// Default background, for clearing cells the snapshot did not mention.
    pub background: Rgba,
}

impl TerminalFrame {
    /// An empty frame, used before a session produces output.
    pub fn empty(columns: u16, rows: u16, background: Rgba) -> Self {
        Self {
            cells: Vec::new(),
            cursor: None,
            columns,
            rows,
            display_offset: 0,
            background,
        }
    }
}

/// Copy the visible grid out of `term`, resolving colors against `theme`.
///
/// `focused` controls cursor rendering: an unfocused pane shows the configured
/// unfocused shape, which is how a user tells at a glance which split has focus.
pub fn snapshot(
    term: &Term<EventProxy>,
    theme: &Theme,
    cfg: &Config,
    focused: bool,
    cursor_visible: bool,
) -> TerminalFrame {
    let content = term.renderable_content();
    let columns = term.columns() as u16;
    let rows = term.screen_lines() as u16;
    let display_offset = content.display_offset;
    let selection = content.selection;

    let mut cells = Vec::with_capacity(columns as usize * rows as usize / 2);

    for item in content.display_iter {
        let cell = item.cell;

        // Spacers are the second half of a wide glyph. The base cell already
        // covers both columns, so drawing them again would double-render.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }

        let point = item.point;
        let row = match viewport_row(point, display_offset, rows) {
            Some(r) => r,
            None => continue,
        };

        let selected = selection.is_some_and(|range| contains(&range, point));

        let CellColors { mut fg, mut bg } =
            color::resolve(theme, content.colors, cell.fg, cell.bg, cell.flags);

        if selected {
            // Selection wins over cell colors, otherwise selected text on a
            // colored background can end up invisible.
            fg = theme.selection_foreground();
            bg = theme.selection_background();
        }

        let is_default_bg = bg == theme.background;
        let blank = cell.c == ' ' || cell.c == '\0';

        // Skip cells that contribute nothing. This is the single biggest win in
        // the snapshot: a mostly-empty screen produces a handful of cells rather
        // than columns*rows of them.
        if blank
            && is_default_bg
            && cell.flags.is_empty()
            && cell.zerowidth().is_none_or(|z| z.is_empty())
        {
            continue;
        }

        cells.push(RenderCell {
            col: point.column.0 as u16,
            row,
            ch: cell.c,
            zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
            fg,
            bg,
            underline_color: cell
                .underline_color()
                .map(|c| color::resolve(theme, content.colors, c, cell.bg, Flags::empty()).fg),
            flags: cell_flags(cell.flags),
        });
    }

    let cursor = render_cursor(
        &content.cursor,
        content.mode,
        display_offset,
        rows,
        theme,
        cfg,
        focused,
        cursor_visible,
    );

    TerminalFrame {
        cells,
        cursor,
        columns,
        rows,
        display_offset,
        background: theme.background,
    }
}

/// The text a shell prompt is showing, left of the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLine {
    /// Cursor row from column 0 up to the cursor. Wide-glyph spacers are dropped,
    /// so one glyph is one `char`.
    pub line: String,
    /// The cursor's column. Not `line.chars().count()`: a double-width glyph is one
    /// `char` and two columns.
    pub cursor_col: u16,
    /// Whether every column from the cursor to the end of the row is blank.
    ///
    /// Answered here because this is the only place that can: `line` stops at the
    /// cursor, so nothing downstream can see what follows it.
    pub at_line_end: bool,
}

/// Read the cursor's row up to the cursor, or `None` when it is not an input line.
///
/// Deliberately not part of [`snapshot`]: that runs for every visible pane every
/// frame, while this is wanted for one pane and only when something asked for it.
///
/// `None` in five cases, each of which would produce a *wrong* answer rather than a
/// missing one:
///
/// - the **alternate screen**, where a row is a full-screen program's canvas and not
///   an input line. This is a privacy property as much as a drawing one: it is why
///   what you type into `vim` or a TUI password box is never reported;
/// - a **scrolled-back** viewport, where the cursor is not where typing lands;
/// - a cursor **outside the visible rows**, for the same reason;
/// - a **continuation row**, where the command began on the row above and this row
///   holds only its tail. A partial prefix yields confidently wrong completions;
/// - a row containing a **`HIDDEN`** cell. SGR 8 is what a password prompt uses when
///   it wants the characters present but invisible (see `color::resolve`), so the
///   text really is in the grid. This check has to live here: `CellFlags` does not
///   carry `HIDDEN`, because downstream it is only ever a color change, so there is
///   no later place to notice.
pub fn input_line(term: &Term<EventProxy>) -> Option<InputLine> {
    if term.mode().contains(TermMode::ALT_SCREEN) {
        return None;
    }

    let grid = term.grid();
    if grid.display_offset() != 0 {
        return None;
    }

    let point = grid.cursor.point;
    let rows = grid.screen_lines() as i32;
    if point.line.0 < 0 || point.line.0 >= rows {
        return None;
    }

    let columns = grid.columns();
    // `WRAPLINE` sits on the last cell of the row that wrapped, so the row *above* is
    // what says this one is a continuation. Guarded against indexing past the top of
    // the scrollback.
    if point.line.0 > -(grid.history_size() as i32) {
        let above = &grid[Line(point.line.0 - 1)];
        if above[Column(columns - 1)].flags.contains(Flags::WRAPLINE) {
            return None;
        }
    }

    let cursor_col = (point.column.0).min(columns);
    let row = &grid[point.line];

    let mut line = String::with_capacity(cursor_col);
    for col in 0..cursor_col {
        let cell = &row[Column(col)];
        if cell.flags.contains(Flags::HIDDEN) {
            return None;
        }
        // The second half of a wide glyph carries no character of its own; pushing it
        // would insert a space into the middle of a CJK word.
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        line.push(cell.c);
    }

    // Whether anything is written to the right of the cursor. Cheap here — the row
    // is already in hand — and impossible anywhere else, since `line` stops at the
    // cursor.
    let at_line_end = (cursor_col..columns).all(|col| {
        let c = row[Column(col)].c;
        c == ' ' || c == '\0'
    });

    // `line` is not trimmed: `git ` and `git` are different prefixes, so a trailing
    // space before the cursor is load-bearing for anything matching against it.
    Some(InputLine {
        line,
        cursor_col: cursor_col as u16,
        at_line_end,
    })
}

/// Where a hint would be drawn: the cursor's `(column, row)` in viewport coordinates.
///
/// Separate from [`RenderCursor`] because that describes the cursor as *drawn*, and is
/// deliberately `None` on the dark half of a blink. Ghost text must not blink with the
/// cursor — the suggestion is not the cursor, and flashing it is unreadable — so it
/// anchors on position, which does not blink.
///
/// Still `None` when the program hid the cursor with DECTCEM, or when the cursor is
/// scrolled out of view. A program that hid its cursor is not sitting at a prompt.
pub fn cursor_anchor(term: &Term<EventProxy>) -> Option<(u16, u16)> {
    if !term.mode().contains(TermMode::SHOW_CURSOR) {
        return None;
    }
    let grid = term.grid();
    let rows = grid.screen_lines() as u16;
    let point = grid.cursor.point;
    let row = viewport_row(point, grid.display_offset(), rows)?;
    Some((point.column.0.min(grid.columns()) as u16, row))
}

/// Append `hint` as dim "ghost" cells starting at `anchor`. Returns the number of
/// columns filled, which is 0 whenever a guard refused.
///
/// Separate from [`snapshot`] on purpose: the snapshot is a faithful copy of the grid
/// and nothing else, so adding cells the terminal never wrote is a step the caller
/// opts into and a reader can find.
///
/// `anchor` is passed in rather than read from [`TerminalFrame::cursor`] so the hint
/// does not inherit the cursor's blink — see [`cursor_anchor`], which is how a caller
/// should obtain it.
///
/// Refuses unless every column from the anchor to the end of its row is empty.
/// [`snapshot`] omits blank default-background cells, so any cell still present at or
/// after the anchor is something visible, and a hint must never hide what a program
/// printed.
///
/// The caller owns the two guards a frame cannot express: that the pane has focus,
/// and that it is not on the alternate screen.
pub fn draw_inline_hint(
    frame: &mut TerminalFrame,
    anchor: (u16, u16),
    hint: &str,
    fg: Rgba,
) -> u16 {
    let (start_col, row) = anchor;
    if hint.is_empty() || frame.display_offset != 0 {
        return 0;
    }
    if frame
        .cells
        .iter()
        .any(|c| c.row == row && c.col >= start_col)
    {
        return 0;
    }

    let mut col = start_col;
    for ch in hint.chars() {
        // A control character would draw as a replacement box, or be mistaken for
        // movement by a reader of this vector.
        if ch.is_control() {
            break;
        }
        let width = match unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) {
            // A combining mark with no base glyph of its own in the hint.
            0 => continue,
            w => w as u16,
        };
        // Truncated at the last column, never wrapped: a hint is speculative, and
        // wrapping one would push real output down a row.
        if col as u32 + width as u32 > frame.columns as u32 {
            break;
        }
        frame.cells.push(RenderCell {
            col,
            row,
            ch,
            zerowidth: Vec::new(),
            fg,
            // The default background, so `build_pane`'s background pass emits no quad:
            // a hint costs one textured instance per glyph and nothing else.
            bg: frame.background,
            underline_color: None,
            flags: CellFlags {
                wide: width == 2,
                ..Default::default()
            },
        });
        col += width;
    }

    col - start_col
}

/// Convert a grid point to a viewport row, or `None` if it is scrolled off.
fn viewport_row(point: Point, display_offset: usize, rows: u16) -> Option<u16> {
    // Grid lines are signed: negative values are scrollback above the viewport.
    let line = point.line.0 + display_offset as i32;
    if line < 0 || line >= rows as i32 {
        return None;
    }
    Some(line as u16)
}

/// Whether a selection covers a point, honoring block selections.
///
/// A block selection constrains columns on every line; a normal selection only
/// constrains the first and last. Conflating the two makes block selection copy
/// whole lines.
fn contains(range: &SelectionRange, point: Point) -> bool {
    if point.line < range.start.line || point.line > range.end.line {
        return false;
    }

    if range.is_block {
        return point.column >= range.start.column && point.column <= range.end.column;
    }

    // Single-line selection: both bounds apply.
    if range.start.line == range.end.line {
        return point.column >= range.start.column && point.column <= range.end.column;
    }
    if point.line == range.start.line {
        return point.column >= range.start.column;
    }
    if point.line == range.end.line {
        return point.column <= range.end.column;
    }
    // A line strictly between the endpoints is fully selected.
    true
}

fn cell_flags(flags: Flags) -> CellFlags {
    // Checked most specific first: DOUBLE_UNDERLINE also sets bits that overlap
    // with the plain UNDERLINE test in some encodings.
    let underline = if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    };

    CellFlags {
        bold: flags.contains(Flags::BOLD),
        italic: flags.contains(Flags::ITALIC),
        strikeout: flags.contains(Flags::STRIKEOUT),
        underline,
        wide: flags.contains(Flags::WIDE_CHAR),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_cursor(
    cursor: &alacritty_terminal::term::RenderableCursor,
    mode: TermMode,
    display_offset: usize,
    rows: u16,
    theme: &Theme,
    cfg: &Config,
    focused: bool,
    visible: bool,
) -> Option<RenderCursor> {
    // The program can hide the cursor (DECTCEM); honoring it matters for
    // full-screen apps that draw their own.
    if !mode.contains(TermMode::SHOW_CURSOR) || !visible {
        return None;
    }
    // Scrolled back into history: showing a cursor there would imply the user can
    // type at that position.
    if display_offset != 0 {
        return None;
    }

    let row = viewport_row(cursor.point, display_offset, rows)?;

    // The program's DECSCUSR choice wins unless the user opted out.
    let shape = if cfg.cursor.allow_program_override {
        from_ansi_shape(cursor.shape).unwrap_or(cfg.cursor.shape)
    } else {
        cfg.cursor.shape
    };
    let shape = if focused {
        shape
    } else {
        cfg.cursor.unfocused_shape
    };

    Some(RenderCursor {
        col: cursor.point.column.0 as u16,
        row,
        shape,
        color: theme.cursor(),
        text_color: theme.cursor_text(),
        thickness: cfg.cursor.thickness,
    })
}

fn from_ansi_shape(shape: AnsiCursorShape) -> Option<CursorShape> {
    Some(match shape {
        AnsiCursorShape::Block => CursorShape::Block,
        AnsiCursorShape::Underline => CursorShape::Underline,
        AnsiCursorShape::Beam => CursorShape::Beam,
        AnsiCursorShape::HollowBlock => CursorShape::HollowBlock,
        // `Hidden` is handled by the SHOW_CURSOR check above.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::index::{Column, Line};

    use crate::session::{Session, TermSize};
    use tuz_layout::PaneId;

    fn session(cols: u16, rows: u16) -> Session {
        Session::detached(PaneId(1), TermSize::new(cols, rows, 8, 16))
    }

    fn snap(s: &Session) -> TerminalFrame {
        let theme = Theme::builtin_default();
        let cfg = Config::default();
        snapshot(&s.term().lock(), &theme, &cfg, true, true)
    }

    fn cell_at(frame: &TerminalFrame, col: u16, row: u16) -> Option<&RenderCell> {
        frame.cells.iter().find(|c| c.col == col && c.row == row)
    }

    fn read_line(s: &Session) -> Option<InputLine> {
        input_line(&s.term().lock())
    }

    fn anchor(s: &Session) -> (u16, u16) {
        cursor_anchor(&s.term().lock()).expect("a visible cursor should have an anchor")
    }

    #[test]
    fn the_input_line_is_the_row_left_of_the_cursor() {
        let s = session(20, 3);
        s.feed_for_test(b"$ git st");

        assert_eq!(
            read_line(&s).expect("a fresh prompt row should be an input line"),
            InputLine {
                line: "$ git st".to_owned(),
                cursor_col: 8,
                at_line_end: true,
            }
        );
    }

    #[test]
    fn a_cursor_inside_the_line_is_not_at_the_end_of_it() {
        // The fact a plugin cannot work out for itself, and the one that decides
        // whether appending a suggestion is safe at all.
        let s = session(20, 3);
        s.feed_for_test(b"ls -la\x1b[3D");

        let read = read_line(&s).unwrap();
        assert_eq!(read.line, "ls ");
        assert!(!read.at_line_end);
    }

    #[test]
    fn the_input_line_stops_at_the_cursor_not_the_end_of_the_row() {
        // Typing in the middle of a line: only what is left of the cursor is a prefix.
        let s = session(20, 3);
        s.feed_for_test(b"abcdef\x1b[3D");

        let read = read_line(&s).expect("a mid-line cursor still has an input line");
        assert_eq!(read.line, "abc");
        assert_eq!(read.cursor_col, 3);
    }

    #[test]
    fn trailing_spaces_before_the_cursor_are_kept() {
        // `git ` and `git` are different prefixes; trimming would suggest against the
        // wrong one.
        let s = session(20, 3);
        s.feed_for_test(b"git  ");

        assert_eq!(read_line(&s).unwrap().line, "git  ");
    }

    #[test]
    fn a_wide_glyph_is_one_char_and_two_columns() {
        // The invariant that justifies carrying `cursor_col` separately at all.
        let s = session(20, 3);
        s.feed_for_test("日本x".as_bytes());

        let read = read_line(&s).unwrap();
        assert_eq!(read.line, "日本x");
        assert_eq!(read.cursor_col, 5);
    }

    #[test]
    fn a_full_screen_program_has_no_input_line() {
        // Named for the privacy property, not the drawing one: this is what keeps
        // what you type into vim or a TUI password box from reaching a plugin.
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[?1049hsecret in a tui");

        assert!(read_line(&s).is_none());
    }

    #[test]
    fn a_row_with_hidden_cells_has_no_input_line() {
        // SGR 8 keeps the characters in the grid and only hides them, which is exactly
        // what a password prompt does. Reading the row would hand over the password.
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[8mhunter2");

        assert!(read_line(&s).is_none());
    }

    #[test]
    fn a_scrolled_back_view_has_no_input_line() {
        let s = session(20, 3);
        s.feed_for_test(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        s.scroll(2);

        assert!(read_line(&s).is_none());
    }

    #[test]
    fn a_wrapped_command_reports_no_input_line() {
        // The cursor row holds only the tail of the command, and a tail is not a
        // prefix — suggesting from it would be confidently wrong.
        let s = session(6, 3);
        s.feed_for_test(b"abcdefgh");

        assert!(read_line(&s).is_none());
    }

    #[test]
    fn a_hint_becomes_cells_starting_at_the_cursor() {
        let s = session(20, 3);
        s.feed_for_test(b"git st");
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "atus", Rgba::rgb(1, 2, 3)),
            4
        );
        // Five, not six, for `git st`: the space is a blank default-background cell,
        // which the snapshot omits. Plus the four hint cells.
        assert_eq!(f.cells.len(), 9);
        assert_eq!(cell_at(&f, 6, 0).unwrap().ch, 'a');
        assert_eq!(cell_at(&f, 6, 0).unwrap().fg, Rgba::rgb(1, 2, 3));
        assert_eq!(cell_at(&f, 9, 0).unwrap().ch, 's');
    }

    #[test]
    fn a_hint_never_covers_what_the_program_printed() {
        // Cursor moved back to column 0 with text still to its right.
        let s = session(20, 3);
        s.feed_for_test(b"ls -la\x1b[6D");
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "atus", Rgba::rgb(1, 2, 3)),
            0
        );
        // `ls -la` is five cells; the space is blank and omitted.
        assert_eq!(f.cells.len(), 5);
    }

    #[test]
    fn a_hint_is_truncated_at_the_last_column_not_wrapped() {
        let s = session(10, 2);
        s.feed_for_test(b"abc");
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "defghijklmnop", Rgba::rgb(1, 2, 3)),
            7
        );
        assert_eq!(f.cells.iter().filter(|c| c.row == 0).count(), 10);
        assert!(f.cells.iter().all(|c| c.row == 0 && c.col < 10));
    }

    #[test]
    fn a_wide_hint_glyph_takes_two_columns_and_is_dropped_rather_than_split() {
        let s = session(5, 2);
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "日本語", Rgba::rgb(1, 2, 3)),
            4
        );
        assert!(cell_at(&f, 0, 0).unwrap().flags.wide);
        assert_eq!(cell_at(&f, 2, 0).unwrap().ch, '本');
        assert!(cell_at(&f, 4, 0).is_none());
    }

    #[test]
    fn a_program_that_hid_its_cursor_has_no_anchor() {
        // DECTCEM. A program that hid its cursor is not sitting at a prompt, so there is
        // nowhere a suggestion belongs — and the caller never gets an anchor to pass.
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[?25lgit st");

        assert!(cursor_anchor(&s.term().lock()).is_none());
    }

    #[test]
    fn a_hint_does_not_blink_with_the_cursor() {
        // The anchor exists so ghost text is not tied to the cursor's blink phase.
        // `snapshot` sets `cursor: None` on the dark half of a blink, and a hint keyed
        // off that flashed on and off — unreadable, and the reason the anchor is a
        // parameter rather than being read back out of the frame.
        let s = session(20, 3);
        s.feed_for_test(b"git st");
        let theme = Theme::builtin_default();
        let cfg = Config::default();

        // The dark half of the blink: the cursor is not drawn.
        let mut dark = snapshot(&s.term().lock(), &theme, &cfg, true, false);
        assert!(dark.cursor.is_none(), "the blink-off frame draws no cursor");

        // The hint is drawn anyway, in exactly the same place as on the lit half.
        assert_eq!(
            draw_inline_hint(&mut dark, anchor(&s), "atus", Rgba::rgb(1, 2, 3)),
            4
        );
        assert_eq!(cell_at(&dark, 6, 0).unwrap().ch, 'a');
    }

    #[test]
    fn an_empty_hint_changes_nothing() {
        let s = session(20, 3);
        s.feed_for_test(b"git st");
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "", Rgba::rgb(1, 2, 3)),
            0
        );
        assert_eq!(f.cells.len(), 5);
    }

    #[test]
    fn a_control_character_ends_a_hint() {
        let s = session(20, 3);
        let mut f = snap(&s);

        assert_eq!(
            draw_inline_hint(&mut f, anchor(&s), "ab\ncd", Rgba::rgb(1, 2, 3)),
            2
        );
        assert!(f.cells.iter().all(|c| c.ch != '\n'));
    }

    #[test]
    fn text_becomes_cells_at_the_right_positions() {
        let s = session(20, 3);
        s.feed_for_test(b"hi");
        let f = snap(&s);

        assert_eq!(cell_at(&f, 0, 0).unwrap().ch, 'h');
        assert_eq!(cell_at(&f, 1, 0).unwrap().ch, 'i');
        assert_eq!((f.columns, f.rows), (20, 3));
    }

    #[test]
    fn blank_default_cells_are_omitted() {
        // The main snapshot optimization: an almost-empty screen must not produce
        // columns*rows cells.
        let s = session(80, 24);
        s.feed_for_test(b"x");
        let f = snap(&s);

        assert_eq!(
            f.cells.len(),
            1,
            "only the non-blank cell should be emitted"
        );
    }

    #[test]
    fn a_colored_blank_is_kept() {
        // A space with a non-default background is visible, so it must survive
        // the blank filter.
        let s = session(20, 3);
        // Red background, then a space.
        s.feed_for_test(b"\x1b[41m ");
        let f = snap(&s);

        let c = cell_at(&f, 0, 0).expect("colored blank must be emitted");
        assert_eq!(c.bg, Theme::builtin_default().normal.red);
    }

    #[test]
    fn sgr_attributes_reach_the_render_flags() {
        let s = session(20, 3);
        // bold, italic, underline, strikeout.
        s.feed_for_test(b"\x1b[1;3;4;9mA");
        let f = snap(&s);

        let c = cell_at(&f, 0, 0).unwrap();
        assert!(c.flags.bold);
        assert!(c.flags.italic);
        assert!(c.flags.strikeout);
        assert_eq!(c.flags.underline, Underline::Single);
    }

    #[test]
    fn underline_styles_are_distinguished() {
        for (seq, expected) in [
            (&b"\x1b[4:1mA"[..], Underline::Single),
            (&b"\x1b[4:2mA"[..], Underline::Double),
            (&b"\x1b[4:3mA"[..], Underline::Curly),
            (&b"\x1b[4:4mA"[..], Underline::Dotted),
            (&b"\x1b[4:5mA"[..], Underline::Dashed),
        ] {
            let s = session(10, 2);
            s.feed_for_test(seq);
            let f = snap(&s);
            assert_eq!(
                cell_at(&f, 0, 0).unwrap().flags.underline,
                expected,
                "for {seq:?}"
            );
        }
    }

    #[test]
    fn truecolor_sgr_is_passed_through_exactly() {
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[38;2;10;20;30mA");
        let f = snap(&s);
        assert_eq!(cell_at(&f, 0, 0).unwrap().fg, Rgba::rgb(10, 20, 30));
    }

    #[test]
    fn wide_glyphs_emit_one_cell_not_two() {
        // A CJK character occupies two columns but is one glyph; emitting the
        // spacer too would draw it twice.
        let s = session(20, 3);
        s.feed_for_test("日本".as_bytes());
        let f = snap(&s);

        assert_eq!(f.cells.len(), 2, "two glyphs, four columns");
        let first = cell_at(&f, 0, 0).unwrap();
        assert_eq!(first.ch, '日');
        assert!(first.flags.wide);
        // The spacer column carries no cell of its own.
        assert!(cell_at(&f, 1, 0).is_none());
        assert_eq!(cell_at(&f, 2, 0).unwrap().ch, '本');
    }

    #[test]
    fn combining_marks_ride_along_with_their_base_glyph() {
        let s = session(20, 3);
        // 'e' followed by a combining acute accent.
        s.feed_for_test("e\u{0301}".as_bytes());
        let f = snap(&s);

        let c = cell_at(&f, 0, 0).unwrap();
        assert_eq!(c.ch, 'e');
        assert_eq!(c.zerowidth, vec!['\u{0301}']);
    }

    #[test]
    fn the_cursor_starts_at_the_origin() {
        let s = session(20, 3);
        let f = snap(&s);
        let cursor = f.cursor.expect("cursor should be visible");
        assert_eq!((cursor.col, cursor.row), (0, 0));
    }

    #[test]
    fn the_cursor_follows_output() {
        let s = session(20, 3);
        s.feed_for_test(b"abc");
        let cursor = snap(&s).cursor.unwrap();
        assert_eq!((cursor.col, cursor.row), (3, 0));
    }

    #[test]
    fn a_hidden_cursor_is_not_rendered() {
        // DECTCEM off: full-screen programs draw their own cursor.
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[?25l");
        assert!(snap(&s).cursor.is_none());
    }

    #[test]
    fn blinking_off_hides_the_cursor_without_touching_the_grid() {
        let s = session(20, 3);
        s.feed_for_test(b"abc");
        let theme = Theme::builtin_default();
        let cfg = Config::default();

        let f = snapshot(&s.term().lock(), &theme, &cfg, true, false);
        assert!(f.cursor.is_none());
        assert!(!f.cells.is_empty(), "text must still render while blinking");
    }

    #[test]
    fn an_unfocused_pane_uses_the_unfocused_cursor_shape() {
        let s = session(20, 3);
        let theme = Theme::builtin_default();
        let mut cfg = Config::default();
        cfg.cursor.shape = CursorShape::Block;
        cfg.cursor.unfocused_shape = CursorShape::HollowBlock;

        let focused = snapshot(&s.term().lock(), &theme, &cfg, true, true);
        assert_eq!(focused.cursor.unwrap().shape, CursorShape::Block);

        let unfocused = snapshot(&s.term().lock(), &theme, &cfg, false, true);
        assert_eq!(unfocused.cursor.unwrap().shape, CursorShape::HollowBlock);
    }

    #[test]
    fn the_program_can_change_the_cursor_shape_unless_disallowed() {
        let s = session(20, 3);
        let theme = Theme::builtin_default();

        // DECSCUSR 5: blinking beam, what vim sets for insert mode.
        s.feed_for_test(b"\x1b[5 q");

        let mut cfg = Config::default();
        cfg.cursor.shape = CursorShape::Block;
        cfg.cursor.allow_program_override = true;
        let f = snapshot(&s.term().lock(), &theme, &cfg, true, true);
        assert_eq!(f.cursor.unwrap().shape, CursorShape::Beam);

        cfg.cursor.allow_program_override = false;
        let f = snapshot(&s.term().lock(), &theme, &cfg, true, true);
        assert_eq!(
            f.cursor.unwrap().shape,
            CursorShape::Block,
            "the user's choice must win when overrides are off"
        );
    }

    #[test]
    fn scrolling_into_history_hides_the_cursor() {
        let s = session(10, 2);
        // Push enough lines that there is history to scroll into.
        s.feed_for_test(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        s.scroll(2);

        let f = snap(&s);
        assert!(f.display_offset > 0, "should be scrolled back");
        assert!(
            f.cursor.is_none(),
            "a cursor in history would imply the user can type there"
        );
    }

    #[test]
    fn scrollback_content_is_visible_after_scrolling_up() {
        let s = session(10, 2);
        s.feed_for_test(b"aaa\r\nbbb\r\nccc\r\n");
        s.scroll_to_top();

        let f = snap(&s);
        let row0: String = (0..3)
            .filter_map(|c| cell_at(&f, c, 0).map(|x| x.ch))
            .collect();
        assert_eq!(row0, "aaa");
    }

    #[test]
    fn rows_outside_the_viewport_are_dropped() {
        let s = session(10, 2);
        s.feed_for_test(b"1\r\n2\r\n3\r\n4\r\n5\r\n");
        let f = snap(&s);
        assert!(
            f.cells.iter().all(|c| c.row < f.rows),
            "every emitted row must be inside the viewport"
        );
    }

    // --- selection --------------------------------------------------------

    fn point(line: i32, col: usize) -> Point {
        Point::new(Line(line), Column(col))
    }

    #[test]
    fn a_single_line_selection_bounds_both_ends() {
        let r = SelectionRange::new(point(0, 2), point(0, 5), false);
        assert!(!contains(&r, point(0, 1)));
        assert!(contains(&r, point(0, 2)));
        assert!(contains(&r, point(0, 5)));
        assert!(!contains(&r, point(0, 6)));
    }

    #[test]
    fn a_multi_line_selection_fills_the_interior_lines() {
        let r = SelectionRange::new(point(0, 5), point(2, 3), false);
        // First line: from the start column to the end of the line.
        assert!(!contains(&r, point(0, 4)));
        assert!(contains(&r, point(0, 5)));
        assert!(contains(&r, point(0, 99)));
        // Interior line: entirely selected.
        assert!(contains(&r, point(1, 0)));
        assert!(contains(&r, point(1, 99)));
        // Last line: up to the end column.
        assert!(contains(&r, point(2, 3)));
        assert!(!contains(&r, point(2, 4)));
    }

    #[test]
    fn a_block_selection_constrains_columns_on_every_line() {
        // Conflating this with a normal selection makes block-copy grab whole
        // lines, which is the classic bug here.
        let r = SelectionRange::new(point(0, 2), point(2, 4), true);
        for line in 0..=2 {
            assert!(!contains(&r, point(line, 1)), "line {line}");
            assert!(contains(&r, point(line, 2)), "line {line}");
            assert!(contains(&r, point(line, 4)), "line {line}");
            assert!(!contains(&r, point(line, 5)), "line {line}");
        }
    }

    #[test]
    fn selection_recolors_cells_over_their_own_colors() {
        let s = session(20, 3);
        s.feed_for_test(b"\x1b[31mred");

        // Select the first two columns.
        {
            use alacritty_terminal::selection::{Selection, SelectionType};
            let mut term = s.term().lock();
            let mut sel = Selection::new(
                SelectionType::Simple,
                point(0, 0),
                alacritty_terminal::index::Side::Left,
            );
            sel.update(point(0, 1), alacritty_terminal::index::Side::Right);
            term.selection = Some(sel);
        }

        let theme = Theme::builtin_default();
        let f = snapshot(&s.term().lock(), &theme, &Config::default(), true, true);

        let c = cell_at(&f, 0, 0).unwrap();
        assert_eq!(c.bg, theme.selection_background());
        assert_eq!(
            c.fg,
            theme.selection_foreground(),
            "selection must override the cell's own red"
        );
        // A cell outside the selection keeps its color.
        assert_eq!(cell_at(&f, 2, 0).unwrap().fg, theme.normal.red);
    }

    #[test]
    fn an_empty_frame_reports_its_geometry() {
        let f = TerminalFrame::empty(80, 24, Rgba::BLACK);
        assert_eq!((f.columns, f.rows), (80, 24));
        assert!(f.cells.is_empty());
        assert!(f.cursor.is_none());
    }
}
