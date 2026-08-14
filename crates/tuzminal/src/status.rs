//! What the status bar says.
//!
//! Building the segment list is a pure function over plain data, so all of it —
//! formatting, which segments appear, and what gets dropped when the window is
//! narrow — is testable without a GPU, a PTY or a window. The caller in `app.rs`
//! does the gathering; this module does the deciding.
//!
//! Segments are [`tuz_plugin_api::StatusSegment`] rather than a new type: it is
//! already exactly `{ text, foreground, background }`, `redraw` already converts that
//! shape into the borrowing `StatusItem` the renderer wants, and a parallel struct
//! would buy nothing but a second conversion.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tuz_config::StatusBar;
use tuz_layout::PaneId;
use tuz_plugin_api::StatusSegment;

/// How often the focused pane's directory is re-read from the operating system.
///
/// Not an update latency: the app redraws on events, never on a timer, and the thing
/// that changes a directory — `cd` — is immediately followed by the shell writing a
/// new prompt, which wakes a redraw anyway. This is a ceiling on `/proc` reads during
/// an output burst, where a `cat` of a large file redraws as fast as it can.
const CWD_REFRESH: Duration = Duration::from_millis(500);

/// The focused pane's working directory, re-read at most every [`CWD_REFRESH`].
///
/// A single entry rather than a map keyed by pane: only the focused pane's directory
/// is ever shown, so a map would cache values nothing reads and would need pruning
/// when panes close. Switching pane invalidates, which is what we want anyway — the
/// new pane's directory has to be read regardless.
#[derive(Debug, Default)]
pub struct CwdCache {
    pane: Option<PaneId>,
    read_at: Option<Instant>,
    path: Option<PathBuf>,
}

impl CwdCache {
    /// The directory for `pane`, reading it again if the entry is stale.
    pub fn get(&mut self, pane: PaneId, pid: Option<u32>, now: Instant) -> Option<&Path> {
        let stale = self.pane != Some(pane)
            || self
                .read_at
                .is_none_or(|at| now.duration_since(at) >= CWD_REFRESH);

        if stale {
            self.pane = Some(pane);
            self.read_at = Some(now);
            self.path = pid.and_then(crate::proc::working_directory);
        }
        self.path.as_deref()
    }
}

/// Everything the bar can report, gathered by the caller so this stays pure.
pub struct StatusInput<'a> {
    /// Working directory of the focused pane, if the OS would say.
    pub directory: Option<&'a Path>,
    pub home: Option<&'a Path>,
    /// The OSC 0/2 title, which is what shells set to the running command. Reused
    /// rather than inspecting the process a second time.
    pub title: Option<&'a str>,
    /// Cursor position, zero-based as the grid stores it. `None` when hidden or
    /// scrolled out of view.
    pub cursor: Option<(u16, u16)>,
    /// Columns and rows of the focused pane.
    pub grid: (u16, u16),
    /// How far the view is scrolled back, in lines. Zero means the bottom.
    pub display_offset: usize,
    pub panes: usize,
    pub tabs: usize,
    pub theme: &'a str,
    pub font_size: f32,
    /// Width of one cell, for measuring segments against the space available.
    pub cell_width: f32,
    /// Room the built-in block has to fit in, after the plugin block took its share.
    pub width: f32,
    pub show: &'a StatusBar,
}

/// Padding either side of a segment's text, matching `chrome::PADDING`.
///
/// Duplicated as a constant rather than shared because the renderer's copy is a
/// drawing detail; this one exists to predict the width so the drop decision is made
/// here, where it can be tested, instead of by the renderer's overflow guard.
const SEGMENT_PADDING: f32 = 8.0;

/// Build the built-in segments, longest-lived information first.
///
/// Order is deliberate: everything after the first entry is dropped before it when
/// space runs out, so the list runs from "what am I looking at" down to "what is this
/// terminal configured as".
pub fn build(input: &StatusInput<'_>) -> Vec<StatusSegment> {
    let mut out = Vec::new();

    if input.show.show_directory {
        if let Some(dir) = input.directory {
            out.push(plain(crate::proc::display_path(dir, input.home)));
        }
        if let Some(running) = running_command(input) {
            out.push(plain(running));
        }
    }

    // Only ever present while scrolled, which is the whole point: it is the segment
    // that explains why output has stopped moving, and noise the rest of the time.
    if input.show.show_scroll && input.display_offset > 0 {
        out.push(plain(format!("↑ {}", input.display_offset)));
    }

    if input.show.show_cursor {
        if let Some((col, row)) = input.cursor {
            // One-based, the way every editor and compiler reports a position. The
            // grid stores it zero-based.
            out.push(plain(format!("{}:{}", row as u32 + 1, col as u32 + 1)));
        }
        out.push(plain(format!("{}×{}", input.grid.0, input.grid.1)));
    }

    if input.show.show_session {
        if input.panes > 1 || input.tabs > 1 {
            out.push(plain(format!(
                "{} {} · {} {}",
                input.panes,
                plural(input.panes, "pane"),
                input.tabs,
                plural(input.tabs, "tab")
            )));
        }
        out.push(plain(format!("{} {}pt", input.theme, trim_size(input.font_size))));
    }

    truncate_to_fit(out, input.cell_width, input.width)
}

/// What the focused pane is running, from its window title.
///
/// Shells commonly set the title to `user@host: ~/dir` or to the running command.
/// The first is the directory we already show, so a title that merely repeats the
/// path is dropped rather than printed twice.
fn running_command(input: &StatusInput<'_>) -> Option<String> {
    let title = input.title?.trim();
    if title.is_empty() {
        return None;
    }
    if let Some(dir) = input.directory {
        let shown = crate::proc::display_path(dir, input.home);
        if title.contains(&shown) || title.ends_with(dir.to_string_lossy().as_ref()) {
            return None;
        }
    }
    Some(title.to_owned())
}

/// A font size without a pointless trailing `.0`: `15pt`, but `15.5pt` when it is.
fn trim_size(size: f32) -> String {
    if (size.fract()).abs() < f32::EPSILON {
        format!("{}", size.trunc() as i64)
    } else {
        format!("{size}")
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

fn plain(text: String) -> StatusSegment {
    StatusSegment {
        text,
        foreground: None,
        background: None,
    }
}

/// Drop whole segments from the end until the rest fit.
///
/// Whole segments, never a partial one: half a path is worse than no path, and the
/// renderer's own overflow guard would otherwise cut one mid-word at whatever pixel
/// it ran out at. Deciding here also means the policy has a test.
fn truncate_to_fit(
    segments: Vec<StatusSegment>,
    cell_width: f32,
    available: f32,
) -> Vec<StatusSegment> {
    let mut used = 0.0;
    let mut out = Vec::with_capacity(segments.len());

    for segment in segments {
        let width = segment.text.chars().count() as f32 * cell_width + SEGMENT_PADDING * 2.0;
        if used + width > available {
            break;
        }
        used += width;
        out.push(segment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: f32 = 8.0;

    fn show_all() -> StatusBar {
        StatusBar {
            enabled: true,
            show_directory: true,
            show_cursor: true,
            show_scroll: true,
            show_session: true,
        }
    }

    fn input<'a>(show: &'a StatusBar, home: &'a Path) -> StatusInput<'a> {
        StatusInput {
            directory: None,
            home: Some(home),
            title: None,
            cursor: Some((0, 0)),
            grid: (80, 24),
            display_offset: 0,
            panes: 1,
            tabs: 1,
            theme: "tuz-dark",
            font_size: 15.0,
            cell_width: CELL,
            // Wide enough that nothing is dropped unless a test says so.
            width: 10_000.0,
            show,
        }
    }

    fn texts(segments: &[StatusSegment]) -> Vec<&str> {
        segments.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn the_scroll_segment_is_absent_at_the_bottom_and_present_above_it() {
        let show = show_all();
        let home = Path::new("/home/tuan");

        let at_bottom = build(&input(&show, home));
        assert!(
            !texts(&at_bottom).iter().any(|t| t.starts_with('↑')),
            "nothing should mention scrolling while at the bottom: {:?}",
            texts(&at_bottom)
        );

        let mut scrolled = input(&show, home);
        scrolled.display_offset = 340;
        assert!(
            texts(&build(&scrolled)).contains(&"↑ 340"),
            "scrolled back, the bar has to say so"
        );
    }

    #[test]
    fn the_cursor_is_reported_one_based() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        let mut i = input(&show, home);
        i.cursor = Some((39, 11));
        // Row 11, column 39 in the grid is line 12, column 40 to a human.
        assert!(texts(&build(&i)).contains(&"12:40"));
    }

    #[test]
    fn a_hidden_cursor_omits_the_position_rather_than_showing_zeros() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        let mut i = input(&show, home);
        i.cursor = None;

        let out = build(&i);
        assert!(
            !texts(&out).iter().any(|t| t.contains(':')),
            "a hidden cursor has no position to report: {:?}",
            texts(&out)
        );
        // The grid size does not depend on the cursor, so it survives.
        assert!(texts(&out).contains(&"80×24"));
    }

    #[test]
    fn a_missing_directory_drops_only_that_segment() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        // The non-Linux case, and the case where the shell has just exited.
        let out = build(&input(&show, home));
        assert!(texts(&out).contains(&"80×24"), "the rest must still be built");
        assert!(!texts(&out).iter().any(|t| t.starts_with('/') || t.starts_with('~')));
    }

    #[test]
    fn a_title_that_only_repeats_the_directory_is_not_shown_twice() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        let mut i = input(&show, home);
        i.directory = Some(Path::new("/home/tuan/hobby/tuzminal"));

        i.title = Some("tuannguyen@fedora:~/hobby/tuzminal");
        let built = build(&i);
        let out = texts(&built);
        assert_eq!(
            out.iter().filter(|t| t.contains("hobby/tuzminal")).count(),
            1,
            "the path should appear once, not once per source: {out:?}"
        );

        // A real command is different information, so it stays.
        i.title = Some("nvim");
        assert!(texts(&build(&i)).contains(&"nvim"));
    }

    #[test]
    fn narrowing_drops_whole_segments_from_the_end() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        let mut i = input(&show, home);
        i.directory = Some(Path::new("/home/tuan/hobby/tuzminal"));
        i.display_offset = 12;

        let full = build(&i);
        assert!(full.len() > 2);

        // Room for roughly the first segment only.
        let first = full[0].text.chars().count() as f32 * CELL + SEGMENT_PADDING * 2.0;
        i.width = first + 4.0;
        let cut = build(&i);

        assert_eq!(cut.len(), 1, "only the first segment fits");
        assert_eq!(
            cut[0].text, full[0].text,
            "and it is unchanged — segments are dropped whole, never trimmed"
        );
    }

    #[test]
    fn no_room_at_all_yields_nothing_rather_than_overflowing() {
        let show = show_all();
        let home = Path::new("/home/tuan");
        let mut i = input(&show, home);
        i.width = 0.0;
        assert!(build(&i).is_empty());
    }

    #[test]
    fn each_group_can_be_turned_off_independently() {
        let home = Path::new("/home/tuan");
        let show = StatusBar {
            enabled: true,
            show_directory: false,
            show_cursor: false,
            show_scroll: false,
            show_session: false,
        };
        let mut i = input(&show, home);
        i.directory = Some(Path::new("/home/tuan/src"));
        i.display_offset = 5;
        assert!(
            build(&i).is_empty(),
            "with every group off the bar has nothing of its own to say"
        );
    }

    #[test]
    fn session_counts_are_only_shown_when_there_is_more_than_one_of_something() {
        let home = Path::new("/home/tuan");
        let show = StatusBar {
            show_directory: false,
            show_cursor: false,
            show_scroll: false,
            show_session: true,
            ..show_all()
        };
        let mut i = input(&show, home);

        // One pane in one tab: the counts would say nothing the window does not.
        assert!(!texts(&build(&i)).iter().any(|t| t.contains("pane")));

        i.panes = 2;
        let out = build(&i);
        assert!(
            texts(&out).iter().any(|t| t.contains("2 panes")),
            "{:?}",
            texts(&out)
        );
    }
}
