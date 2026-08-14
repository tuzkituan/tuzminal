//! A terminal session: one PTY, one child process, one grid of cells.
//!
//! Each pane owns a [`Session`]. The terminal state lives behind a `FairMutex`
//! shared with a background thread that owns the PTY: that thread reads bytes,
//! feeds the VT parser, and wakes the UI. Writes go the other way through a
//! [`Notifier`] channel.
//!
//! ```text
//!   child process
//!        │  ▲
//!    pty │  │ Msg::Input
//!        ▼  │
//!  ┌──────────────────┐   feeds    ┌──────────────────────┐
//!  │ alacritty        │───────────►│ Term (FairMutex)     │
//!  │ event_loop thread│            └──────────────────────┘
//!  └──────────────────┘                      ▲ reads on redraw
//!        │ Event                             │
//!        ▼                             ┌───────────┐
//!   event channel ────── wake ────────►│ UI thread │
//!                                      └───────────┘
//! ```
//!
//! The `FairMutex` matters: a plain mutex lets the PTY thread starve the UI under
//! heavy output (`cat` of a large file), which shows up as an unresponsive window
//! exactly when the user wants to hit Ctrl-C.

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
#[cfg(any(test, feature = "test-util"))]
use alacritty_terminal::vte::ansi::Processor;
use alacritty_terminal::vte::ansi::{ClearMode, Handler};
#[cfg(any(test, feature = "test-util"))]
use alacritty_terminal::Grid;
use std::borrow::Cow;
use std::sync::Arc;
use tuz_config::Config;
use tuz_layout::PaneId;

/// Terminal dimensions in cells, plus the pixel size of one cell.
///
/// The pixel fields are not decoration: programs read them via `TIOCGWINSZ` to
/// size inline images and sixel output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

/// Smallest grid we will ever report.
///
/// A zero dimension in `TIOCSWINSZ` makes programs behave erratically, and a
/// *single-column* grid panics inside `alacritty_terminal`: the cursor is allowed
/// to advance to column 1, which is then out of bounds. Two columns is the
/// smallest width that cannot trigger it.
const MIN_COLUMNS: u16 = 2;
const MIN_LINES: u16 = 1;

impl TermSize {
    /// Clamps to a grid that is safe to hand to the VT parser and the child.
    pub fn new(columns: u16, screen_lines: u16, cell_width: u16, cell_height: u16) -> Self {
        Self {
            columns: columns.max(MIN_COLUMNS) as usize,
            screen_lines: screen_lines.max(MIN_LINES) as usize,
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        }
    }

    fn window_size(&self) -> WindowSize {
        WindowSize {
            num_lines: self.screen_lines as u16,
            num_cols: self.columns as u16,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// An event from a pane's terminal, tagged with which pane produced it.
#[derive(Clone)]
pub struct PaneEvent {
    pub pane: PaneId,
    pub event: AlacrittyEvent,
}

impl std::fmt::Debug for PaneEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // AlacrittyEvent holds `Arc<dyn Fn>` in some variants and is not Debug in
        // a useful way, so print the discriminant only.
        write!(f, "PaneEvent({}, {})", self.pane, event_name(&self.event))
    }
}

/// Short name for an event, for logging.
pub fn event_name(event: &AlacrittyEvent) -> &'static str {
    use AlacrittyEvent::*;
    match event {
        MouseCursorDirty => "MouseCursorDirty",
        Title(_) => "Title",
        ResetTitle => "ResetTitle",
        ClipboardStore(..) => "ClipboardStore",
        ClipboardLoad(..) => "ClipboardLoad",
        ColorRequest(..) => "ColorRequest",
        PtyWrite(_) => "PtyWrite",
        TextAreaSizeRequest(..) => "TextAreaSizeRequest",
        CursorBlinkingChange => "CursorBlinkingChange",
        Wakeup => "Wakeup",
        Bell => "Bell",
        Exit => "Exit",
        ChildExit(_) => "ChildExit",
    }
}

/// Bridges `alacritty_terminal`'s event callback onto our channel.
///
/// Cloned into the PTY thread, so both sides must be `Send`. The waker exists
/// because the UI thread blocks in the compositor's event loop and would not
/// notice a channel send on its own.
#[derive(Clone)]
pub struct EventProxy {
    pane: PaneId,
    tx: crossbeam_channel::Sender<PaneEvent>,
    waker: Arc<dyn Fn() + Send + Sync>,
}

impl EventProxy {
    pub fn new(
        pane: PaneId,
        tx: crossbeam_channel::Sender<PaneEvent>,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self { pane, tx, waker }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: AlacrittyEvent) {
        // A closed channel means the UI is shutting down; dropping the event is
        // correct, and erroring here would just noise up the logs during exit.
        if self
            .tx
            .send(PaneEvent {
                pane: self.pane,
                event,
            })
            .is_ok()
        {
            (self.waker)();
        }
    }
}

/// The PTY I/O thread's handle.
///
/// `EventLoop::spawn` hands the loop and its state back on join; both are dropped,
/// but the type has to be named to store the handle.
type IoThread = std::thread::JoinHandle<(
    EventLoop<tty::Pty, EventProxy>,
    alacritty_terminal::event_loop::State,
)>;

/// A live terminal: PTY, child process, and grid.
pub struct Session {
    pane: PaneId,
    term: Arc<FairMutex<Term<EventProxy>>>,
    /// `None` for a detached session, which has no PTY. Writes are dropped
    /// rather than the type carrying a fake channel that silently goes nowhere.
    notifier: Option<Notifier>,
    size: TermSize,
    /// Set when the child exits, so the pane can be closed and further writes
    /// skipped rather than erroring on a dead PTY.
    child_exited: bool,
    /// Kept so the PTY thread can be joined on shutdown. The handle yields the
    /// event loop and its state back, which we discard.
    io_thread: Option<IoThread>,
}

impl Session {
    /// Spawn a shell and start pumping its PTY.
    pub fn spawn(
        pane: PaneId,
        cfg: &Config,
        size: TermSize,
        events: crossbeam_channel::Sender<PaneEvent>,
        waker: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, SessionError> {
        let proxy = EventProxy::new(pane, events, waker);

        let pty_options = tty::Options {
            shell: cfg
                .shell
                .program
                .as_ref()
                .map(|program| tty::Shell::new(program.clone(), cfg.shell.args.clone())),
            working_directory: working_directory(cfg),
            // Without this, output written just before the child exits is lost,
            // which truncates the last lines of a script.
            drain_on_exit: true,
            env: pty_env(cfg),
            #[cfg(windows)]
            escape_args: true,
        };

        let term_config = TermConfig {
            scrolling_history: cfg.scrollback.lines as usize,
            ..TermConfig::default()
        };

        let term = Term::new(term_config, &size, proxy.clone());
        let term = Arc::new(FairMutex::new(term));

        // The window id distinguishes panes in the child's environment
        // (WINDOWID); pane ids are unique and never reused, so they serve.
        let pty =
            tty::new(&pty_options, size.window_size(), pane.0 as u64).map_err(SessionError::Pty)?;

        let event_loop = EventLoop::new(term.clone(), proxy, pty, false, false)
            .map_err(SessionError::EventLoop)?;
        let notifier = Notifier(event_loop.channel());
        let io_thread = event_loop.spawn();

        log::debug!("{pane}: spawned {}x{}", size.columns, size.screen_lines);

        Ok(Self {
            pane,
            term,
            notifier: Some(notifier),
            size,
            child_exited: false,
            io_thread: Some(io_thread),
        })
    }

    /// Build a session with no PTY, for tests and for rendering checks.
    ///
    /// Feed it bytes with [`Session::feed_for_test`].
    #[cfg(any(test, feature = "test-util"))]
    pub fn detached(pane: PaneId, size: TermSize) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        // Leak the receiver so `send_event` keeps succeeding; a detached session
        // exists to exercise the parser, and dropping events is the point.
        std::mem::forget(rx);
        let proxy = EventProxy::new(pane, tx, Arc::new(|| {}));
        let term = Term::new(TermConfig::default(), &size, proxy);
        Self {
            pane,
            term: Arc::new(FairMutex::new(term)),
            notifier: None,
            size,
            child_exited: false,
            io_thread: None,
        }
    }

    pub fn pane(&self) -> PaneId {
        self.pane
    }
    pub fn size(&self) -> TermSize {
        self.size
    }
    pub fn child_exited(&self) -> bool {
        self.child_exited
    }
    pub fn mark_child_exited(&mut self) {
        self.child_exited = true;
    }

    /// The shared terminal state. Lock it for as short a time as possible: the
    /// PTY thread is contending for the same mutex.
    pub fn term(&self) -> &Arc<FairMutex<Term<EventProxy>>> {
        &self.term
    }

    /// Send bytes to the child process.
    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.child_exited {
            log::trace!("{}: dropping write to an exited child", self.pane);
            return;
        }
        if let Some(notifier) = &self.notifier {
            notifier.notify(bytes.into().into_owned());
        }
    }

    /// Resize the grid and inform the child.
    ///
    /// Both halves are required and in this order: resizing `Term` first means
    /// the reflowed grid is ready before the child redraws for the new size.
    pub fn resize(&mut self, size: TermSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        if let Some(notifier) = &self.notifier {
            let _ = notifier.0.send(Msg::Resize(size.window_size()));
        }
    }

    /// Change the scrollback capacity without restarting the session.
    pub fn set_scrollback(&self, lines: u32) {
        let mut term = self.term.lock();
        let config = TermConfig {
            scrolling_history: lines as usize,
            ..TermConfig::default()
        };
        term.set_options(config);
    }

    /// Scroll the viewport by `delta` lines; positive scrolls back into history.
    pub fn scroll(&self, delta: i32) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_top(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Top);
    }

    pub fn scroll_to_bottom(&self) {
        use alacritty_terminal::grid::Scroll;
        self.term.lock().scroll_display(Scroll::Bottom);
    }

    /// Scroll by whole screens, for PageUp/PageDown.
    pub fn scroll_page(&self, pages: i32) {
        let lines = self.size.screen_lines as i32 * pages;
        self.scroll(lines);
    }

    /// Drop scrollback history, keeping the visible screen.
    pub fn clear_scrollback(&self) {
        // `clear_screen` comes from the VT `Handler` trait that `Term` implements.
        self.term.lock().clear_screen(ClearMode::Saved);
    }

    /// Select the entire buffer, scrollback included.
    ///
    /// Spans from the topmost line of history to the last column of the bottom line,
    /// so `copy` afterwards yields everything the terminal is holding — not just the
    /// visible screen, which is what makes the action worth having.
    pub fn select_all(&self) {
        use alacritty_terminal::index::{Column, Point, Side};
        use alacritty_terminal::selection::{Selection, SelectionType};

        let mut term = self.term.lock();
        let topmost = term.grid().topmost_line();
        let bottommost = term.grid().bottommost_line();
        let last_column = Column(term.grid().columns().saturating_sub(1));

        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(topmost, Column(0)),
            Side::Left,
        );
        selection.update(Point::new(bottommost, last_column), Side::Right);
        term.selection = Some(selection);
    }

    /// Clear any selection.
    pub fn clear_selection(&self) {
        self.term.lock().selection = None;
    }

    /// The current selection as text, if any.
    pub fn selection_text(&self) -> Option<String> {
        self.term.lock().selection_to_string()
    }

    /// Feed bytes directly into the parser, bypassing the PTY.
    ///
    /// For tests only — the real path is the PTY thread.
    #[cfg(any(test, feature = "test-util"))]
    pub fn feed_for_test(&self, bytes: &[u8]) {
        // The default `StdSyncHandler` timeout is what the real PTY path uses.
        let mut parser = Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new();
        let mut term = self.term.lock();
        for &byte in bytes {
            parser.advance(&mut *term, &[byte]);
        }
    }

    /// Ask the PTY thread to shut down and wait for it.
    ///
    /// Called on pane close so the child is reaped rather than left orphaned.
    pub fn shutdown(&mut self) {
        if let Some(notifier) = &self.notifier {
            let _ = notifier.0.send(Msg::Shutdown);
        }
        if let Some(handle) = self.io_thread.take() {
            // A short wait is enough for a cooperative shutdown; blocking the UI
            // indefinitely on a wedged child would be worse than leaking it.
            if handle.join().is_err() {
                log::warn!("{}: PTY thread panicked during shutdown", self.pane);
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.io_thread.is_some() {
            self.shutdown();
        }
    }
}

/// Resolve the working directory for a new session.
fn working_directory(cfg: &Config) -> Option<std::path::PathBuf> {
    match cfg.shell.working_directory.as_deref() {
        // `inherit_pane` is handled by the caller, which knows the focused pane's
        // cwd; treating it as a literal path here would try to `cd` into a
        // directory named "inherit_pane".
        None | Some("inherit_pane") => None,
        Some(path) => Some(std::path::PathBuf::from(path)),
    }
}

/// Environment for the child process.
fn pty_env(cfg: &Config) -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> =
        cfg.shell.env.clone().into_iter().collect();
    // TERM must be something the child's terminfo knows, or curses programs
    // refuse to start. The config default is `xterm-256color` for that reason.
    env.insert("TERM".to_owned(), cfg.shell.term.clone());
    env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    env.insert("TERM_PROGRAM".to_owned(), "tuzminal".to_owned());
    env.insert(
        "TERM_PROGRAM_VERSION".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    env
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to open a pseudoterminal")]
    Pty(#[source] std::io::Error),
    #[error("failed to start the terminal I/O thread")]
    EventLoop(#[source] std::io::Error),
}

/// Read-only view of a grid, for tests that assert on rendered content.
#[cfg(any(test, feature = "test-util"))]
pub fn grid_to_strings(grid: &Grid<alacritty_terminal::term::cell::Cell>) -> Vec<String> {
    let mut out = Vec::new();
    for line in 0..grid.screen_lines() {
        let line = alacritty_terminal::index::Line(line as i32);
        let mut s = String::new();
        for col in 0..grid.columns() {
            s.push(grid[line][alacritty_terminal::index::Column(col)].c);
        }
        out.push(s.trim_end().to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(cols: u16, rows: u16) -> TermSize {
        TermSize::new(cols, rows, 8, 16)
    }

    #[test]
    fn term_size_clamps_degenerate_dimensions() {
        // A zero dimension confuses child processes, and a single column panics
        // inside alacritty_terminal's cursor handling.
        let s = TermSize::new(0, 0, 0, 0);
        assert_eq!(s.columns, MIN_COLUMNS as usize);
        assert_eq!(s.screen_lines, MIN_LINES as usize);
        assert_eq!(s.cell_width, 1);
        assert_eq!(s.cell_height, 1);

        assert_eq!(
            TermSize::new(1, 1, 8, 16).columns,
            2,
            "1 column must be widened"
        );
    }

    #[test]
    fn a_one_column_grid_does_not_panic_the_parser() {
        // Regression: a tiny window produced a 1-column grid and the PTY thread
        // panicked with "index out of bounds: the len is 1 but the index is 1".
        let s = Session::detached(PaneId(1), TermSize::new(1, 1, 8, 16));
        s.feed_for_test(b"abc");
        let term = s.term().lock();
        assert!(term.grid().columns() >= 2);
    }

    #[test]
    fn term_size_reports_pixel_geometry_to_the_child() {
        // Programs read the pixel fields to size sixel and inline images.
        let ws = size(80, 24).window_size();
        assert_eq!((ws.num_cols, ws.num_lines), (80, 24));
        assert_eq!((ws.cell_width, ws.cell_height), (8, 16));
    }

    #[test]
    fn dimensions_are_reported_consistently() {
        let s = size(100, 30);
        assert_eq!(s.columns(), 100);
        assert_eq!(s.screen_lines(), 30);
    }

    #[test]
    fn env_advertises_truecolor_and_the_configured_term() {
        let mut cfg = Config::default();
        cfg.shell.term = "xterm-256color".to_owned();
        cfg.shell.env.insert("EDITOR".to_owned(), "nvim".to_owned());

        let env = pty_env(&cfg);
        assert_eq!(env["TERM"], "xterm-256color");
        assert_eq!(env["COLORTERM"], "truecolor");
        assert_eq!(env["TERM_PROGRAM"], "tuzminal");
        assert_eq!(env["EDITOR"], "nvim", "user env must be preserved");
    }

    #[test]
    fn term_cannot_be_overridden_into_something_broken() {
        // A user setting TERM in [shell.env] would otherwise silently defeat the
        // `term` setting and could break curses programs.
        let mut cfg = Config::default();
        cfg.shell.term = "xterm-256color".to_owned();
        cfg.shell.env.insert("TERM".to_owned(), "dumb".to_owned());

        assert_eq!(
            pty_env(&cfg)["TERM"],
            "xterm-256color",
            "the `term` setting must win over [shell.env]"
        );
    }

    #[test]
    fn inherit_pane_is_not_treated_as_a_literal_path() {
        let mut cfg = Config::default();
        cfg.shell.working_directory = Some("inherit_pane".to_owned());
        assert_eq!(working_directory(&cfg), None);

        cfg.shell.working_directory = Some("/tmp".to_owned());
        assert_eq!(
            working_directory(&cfg),
            Some(std::path::PathBuf::from("/tmp"))
        );
    }

    #[test]
    fn a_detached_session_parses_plain_text() {
        let s = Session::detached(PaneId(1), size(20, 3));
        s.feed_for_test(b"hello");

        let term = s.term().lock();
        let lines = grid_to_strings(term.grid());
        assert_eq!(lines[0], "hello");
    }

    #[test]
    fn a_detached_session_handles_control_sequences() {
        let s = Session::detached(PaneId(1), size(20, 3));
        // Cursor to row 2 col 3, then write.
        s.feed_for_test(b"\x1b[2;3Hxy");

        let term = s.term().lock();
        let lines = grid_to_strings(term.grid());
        assert_eq!(lines[1], "  xy", "cursor addressing should be honored");
    }

    #[test]
    fn resizing_reflows_the_grid_and_is_idempotent() {
        let mut s = Session::detached(PaneId(1), size(20, 3));
        s.feed_for_test(b"hello");

        s.resize(size(40, 10));
        assert_eq!(s.size().columns, 40);
        assert_eq!(s.term().lock().grid().columns(), 40);

        // Content survives the resize.
        assert_eq!(grid_to_strings(s.term().lock().grid())[0], "hello");

        // A no-op resize must not touch the grid.
        s.resize(size(40, 10));
        assert_eq!(s.size().columns, 40);
    }

    #[test]
    fn writes_to_an_exited_child_are_dropped_not_fatal() {
        let mut s = Session::detached(PaneId(1), size(20, 3));
        s.mark_child_exited();
        assert!(s.child_exited());
        // Must not panic: a keystroke arriving after the shell exits is normal.
        s.write(b"x".to_vec());
    }
}

#[cfg(test)]
mod select_all_tests {
    use super::*;

    fn session(cols: u16, rows: u16) -> Session {
        Session::detached(PaneId(1), TermSize::new(cols, rows, 8, 16))
    }

    #[test]
    fn select_all_captures_the_visible_screen() {
        let s = session(20, 3);
        s.feed_for_test(b"hello\r\nworld");
        s.select_all();

        let text = s.selection_text().expect("something should be selected");
        assert!(text.contains("hello"), "got {text:?}");
        assert!(text.contains("world"), "got {text:?}");
    }

    #[test]
    fn select_all_reaches_into_scrollback() {
        // The reason the action is worth having: selecting only the visible screen
        // is what dragging already does.
        let s = session(20, 2);
        s.feed_for_test(b"oldest\r\nmiddle\r\nnewest\r\n");
        s.select_all();

        let text = s.selection_text().expect("something should be selected");
        assert!(
            text.contains("oldest"),
            "scrolled-off content should be included, got {text:?}"
        );
    }

    #[test]
    fn select_all_includes_the_last_column() {
        // An off-by-one on the end column silently truncates every line.
        let s = session(5, 2);
        s.feed_for_test(b"abcde");
        s.select_all();

        let text = s.selection_text().unwrap();
        assert!(text.contains("abcde"), "got {text:?}");
    }

    #[test]
    fn clearing_removes_the_selection() {
        let s = session(20, 3);
        s.feed_for_test(b"hi");
        s.select_all();
        assert!(s.selection_text().is_some());

        s.clear_selection();
        assert!(s.selection_text().is_none());
    }

    #[test]
    fn select_all_on_an_empty_terminal_does_not_panic() {
        let s = session(20, 3);
        s.select_all();
        // Whitespace or nothing, but it must not crash on a grid of blanks.
        let _ = s.selection_text();
    }
}
