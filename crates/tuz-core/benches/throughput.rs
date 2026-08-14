//! VT parsing throughput.
//!
//! The number that matters for `cat` of a large file: how fast bytes get from the
//! PTY into the grid. Everything else in a frame is bounded by the display refresh;
//! this is not.
//!
//! Deliberately measures the parser and grid alone, with no PTY and no GPU. A
//! benchmark that included them would mostly measure the kernel and the compositor,
//! and would not tell you whether a change to the terminal code made things worse.
//!
//! Run with `cargo bench -p tuz-core --features test-util`.

use std::hint::black_box;
use std::time::Instant;
use tuz_core::{Session, TermSize};
use tuz_layout::PaneId;

/// Rows and columns used throughout, roughly a maximised window.
const COLUMNS: u16 = 200;
const ROWS: u16 = 50;

fn session() -> Session {
    Session::detached(PaneId(1), TermSize::new(COLUMNS, ROWS, 8, 16))
}

/// Time `f` and report throughput.
fn bench(name: &str, bytes: &[u8], iterations: usize) {
    // One warm-up pass so the first measurement is not paying for lazy allocation
    // inside the grid.
    {
        let s = session();
        s.feed_for_test(bytes);
    }

    let start = Instant::now();
    for _ in 0..iterations {
        let s = session();
        s.feed_for_test(black_box(bytes));
        black_box(&s);
    }
    let elapsed = start.elapsed();

    let total = bytes.len() * iterations;
    let mib = total as f64 / (1024.0 * 1024.0);
    let seconds = elapsed.as_secs_f64();

    println!(
        "{name:<28} {:>8.1} MiB/s   ({total} bytes in {:.3}s)",
        mib / seconds,
        seconds
    );
}

/// Plain ASCII lines, the `cat a log file` case.
fn plain_text(lines: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines * 80);
    for i in 0..lines {
        out.extend_from_slice(
            format!("{i:06} the quick brown fox jumps over the lazy dog\r\n").as_bytes(),
        );
    }
    out
}

/// Heavily coloured output, the `ls --color` / build-log case. Colour changes force
/// the parser through the SGR path on nearly every cell.
fn colored_text(lines: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(lines * 200);
    for i in 0..lines {
        for word in 0..8 {
            out.extend_from_slice(
                format!("\x1b[3{}m word{word} \x1b[0m", (word % 8) + 1).as_bytes(),
            );
        }
        out.extend_from_slice(format!(" {i}\r\n").as_bytes());
    }
    out
}

/// Cursor addressing and clears, the full-screen-application case.
fn cursor_heavy(frames: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..frames {
        out.extend_from_slice(b"\x1b[2J\x1b[H");
        for row in 1..=ROWS {
            out.extend_from_slice(format!("\x1b[{row};1Hrow {row} of the display").as_bytes());
        }
    }
    out
}

/// Wide characters, which take the double-width path and a fallback font lookup.
fn wide_text(lines: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..lines {
        out.extend_from_slice("日本語のテキストが並んでいます\r\n".as_bytes());
    }
    out
}

fn main() {
    println!("tuz-core VT throughput ({COLUMNS}x{ROWS} grid)\n");

    bench("plain ascii", &plain_text(2_000), 20);
    bench("sgr colour changes", &colored_text(2_000), 20);
    bench("cursor addressing", &cursor_heavy(200), 20);
    bench("wide characters", &wide_text(2_000), 20);

    println!(
        "\nCompare against another terminal with:\n  \
         time cat <large-file>\n\
         Note this measures parsing only — no PTY read, no rendering."
    );
}
