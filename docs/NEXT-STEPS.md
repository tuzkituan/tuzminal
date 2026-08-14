# Next steps

Deferred work, with enough context to pick any item up cold. Ordered roughly by
value per unit of effort.

Everything in `docs/BUILD-LOG.md` is already done and shipped. This file is only
what is *not*.

---

## Needs a human at the keyboard

None of this can be verified from a script — it needs someone clicking and typing in
a running window. The code paths are unit-tested and the panel is GPU-tested for
drawing, but the interactions themselves have never had a real pointer on them.

1. `+` opens a tab; the strip appears at two tabs; `×` on a hovered tab closes it.
2. Split buttons split; `⚙` opens the panel; `ctrl+shift+,` toggles it.
3. In the panel: change theme → terminal recolors immediately; change font size →
   panes re-grid and `stty size` inside a shell agrees.
4. Tab/Shift+Tab move the focus ring, arrows adjust a stepper, Enter toggles,
   Escape closes, a click outside dismisses.
5. **Save, then `cat ~/.config/tuzminal/config.toml`** — the changed value is there
   and every comment survived. This is the one with real downside if wrong; it is
   covered by tests, but worth seeing once on a real file.
6. Set `decorations = false`, restart, confirm window controls appear and work, then
   set it back and confirm they disappear rather than duplicating the compositor's.

---

## Panel gaps worth closing

**A text-input widget.** Deliberately skipped: a cursor, selection, clipboard and
eventually IME is a lot for one setting, and the font picker is better as a dropdown
regardless. But `[shell] program`, `args` and `env` cannot be edited without one, and
neither can a custom theme name. If it gets built, it should live in `tuz-ui`
alongside the other widgets and reuse the same `UiAction` channel.

**More settings.** The panel exposes a subset by design. `[keys]`, `[shell]` and
`[plugins]` were left out as better hand-edited, but a keybinding editor is the one
most people would actually want, and it needs the chord-capture UI that does not
exist yet.

**Tooltips.** `ChromeButton::describe` already returns the text; nothing displays it.
The tab strip buttons are glyphs with no labels, which is fine for `+` and `×` and
much less obvious for the split buttons.

**Tab reordering.** Dragging a tab to a new position. `Layout::tabs` is a `Vec`, so
the model supports it; it needs drag state in `app.rs` and an insertion indicator.

---

## Rendering

**Subpixel antialiasing.** `grayscale_antialiasing` is in the schema and documented
as having no effect. Real subpixel AA needs three-channel coverage in the glyph atlas
and a dual-source blend in the shader, so it touches `tuz-font`'s rasterization,
the atlas format, and `cell.wgsl`.

**Damage tracking.** `performance.damage_tracking` is honored to the extent that the
loop is event-driven and idles at zero CPU, but a redraw currently rebuilds every
instance for every visible pane. `alacritty_terminal` exposes `Term::damage()` with
per-line bounds, which would let the instance buffer be updated in place for the
lines that changed. Worth doing before claiming the performance targets in the plan;
worth measuring first, because at terminal sizes the rebuild may already be cheap.

**Benchmarks.** `benches/` was in the original plan and never written. The useful
ones: `cat` of a large file compared against `alacritty` on the same machine, the
`vtebench` suite, and a frame-time histogram under continuous output asserting p99
stays inside the vsync interval.

---

## Platform

**macOS and Windows have never been built or run.** This is the largest honest gap.
The abstractions are in place and `alacritty_terminal` supplies ConPTY, so the
expectation is compile errors rather than redesign, but that expectation is untested.
Concrete unknowns:

- `arboard` clipboard behaviour on each platform.
- Whether `fontdb`'s system-font loading finds the right families on macOS.
- Window decorations and the `decorations = false` path, which is where our own
  window controls become load-bearing.
- The `#[cfg(windows)] escape_args` field in `tty::Options` is set but never
  exercised.

A CI matrix building all three targets would catch most of it without a physical
machine.

**IME.** Wayland `text-input-v3` for CJK input. Genuinely involved, and the reason
`keys::bytes_for_key` prefers the platform's composed text — that path is the hook a
real IME implementation would extend rather than replace.

---

## Smaller loose ends

- `select_all` is a named action that logs "not implemented yet".
- `Command::SetConfigOverlay` from a plugin is accepted and logged, not applied.
- `Notify` from a plugin goes to the log; there is no on-screen notification surface.
- No `LICENSE` files, though `Cargo.toml` declares `MIT OR Apache-2.0`.
- The release profile sets `debug = 1` for readable perf profiles, which makes the
  binary 156 MB; `strip` takes it to 23 MB. Fine as a default, worth knowing.
- The registry at `github.com/tuzminal/registry` does not exist, so
  `tuzminal plugin install <name>` only works with a git URL until it does.

---

## Where things live

| Area | Files |
|---|---|
| Widget model, focus, hit-testing | `crates/tuz-ui/src/lib.rs` |
| Widget and panel drawing | `crates/tuz-render/src/widget.rs` |
| Tab strip and buttons | `crates/tuz-render/src/chrome.rs`, `crates/tuz-layout/src/lib.rs` |
| Panel state and wiring | `crates/tuzminal/src/settings.rs`, `crates/tuzminal/src/app.rs` |
| Saving without clobbering comments | `crates/tuz-config/src/save.rs` |
| GPU pixel-readback tests | `crates/tuzminal/tests/render.rs` |
