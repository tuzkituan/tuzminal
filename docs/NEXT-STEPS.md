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
7. **Steppers and selects: click the `‹` and the `›` you can see.** Each arrow now sits
   inside the half of the value column that acts on it; while the arrows travelled with the
   right-aligned value, clicking the visible `‹` incremented.
8. Type at a prompt with matching history: dim ghost text appears, `→` at end of line takes
   it, `alt+→` takes a word. Then open `vim` and confirm no ghost text at all, and scroll
   back and confirm it disappears rather than floating over history.
9. On a **light** theme, check the dropdown's shortcut column, inactive tab titles and the
   status bar are all legible. Those were palette slots that only worked on dark themes.
10. With the file explorer focused, switch to the Settings tab and type: keys must reach the
    page, not an invisible explorer. Then switch back and confirm focus is where you left it.

---

## Panel gaps worth closing

**More settings.** The panel exposes a subset by design. `[keys]`, `[shell]` and
`[plugins]` were left out as better hand-edited, but a keybinding editor is the one
most people would actually want, and it needs the chord-capture UI that does not
exist yet.

**A page cannot share a tab with a shell.** Settings, shortcuts and plugins are whole tabs,
so they cannot sit beside a split or the file explorer. Splitting one is refused outright
(`App::split`) and the sidebar is given zero width there, both deliberately — the honest
alternative to a page drawing into half a tab with an unreachable shell in the other half,
which is what happened before the guard existed.

The obstacle is that `TabKind` is a property of the **tab**, not the pane
(`tuz-layout/src/lib.rs`), so "page beside a shell" is unrepresentable and every downstream
check asks a tab-level question — who owns the keyboard, whether a status bar means
anything, whether a sidebar is useful — to answer a pane-level one. Opening it up means:

1. moving the tag to the leaf (`Node::Leaf(PaneId, PaneKind)`, or a map on `Layout`), and
   spawning sessions only for terminal leaves;
2. keying each page object by its owning `PaneId` instead of `frame.panes.first()`, which is
   how all three pages currently find their rect;
3. making `settings_active()` and friends ask "does the *focused pane* hold this page", and
   letting unclaimed chords fall through to `dispatch` instead of returning unconditionally.

`tuz-layout` needs no other change — the split tree is already kind-agnostic — and the
sidebar needs none at all beyond deleting its guard, since the layout already carves it out
of the pane body independently of what the panes contain.

---

## Rendering

**Parser throughput is worth a look.** `cargo bench -p tuz-core --features test-util`
reports roughly 23 MiB/s of plain ASCII and 11 MiB/s of cursor-addressed output on
this machine. That is enough that `cat` of a large file is not painful, but it is not
obviously competitive; nobody has profiled where the time goes, and the benchmark
exists precisely so a change can be measured rather than guessed at.

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

---

## Platform

**macOS and Windows have never been built or run** on a real machine. A CI matrix
now builds and tests all three targets on every push
(`.github/workflows/ci.yml`), which is the closest thing to closing this gap without
hardware — but until that workflow has actually run green, cross-platform support
stays a claim rather than a fact.
The abstractions are in place and `alacritty_terminal` supplies ConPTY, so the
expectation is compile errors rather than redesign, but that expectation is untested.
Concrete unknowns:

- `arboard` clipboard behaviour on each platform.
- Whether `fontdb`'s system-font loading finds the right families on macOS.
- Window decorations and the `decorations = false` path, which is where our own
  window controls become load-bearing.
- The `#[cfg(windows)] escape_args` field in `tty::Options` is set but never
  exercised.

**IME.** Wayland `text-input-v3` for CJK input. Genuinely involved, and the reason
`keys::bytes_for_key` prefers the platform's composed text — that path is the hook a
real IME implementation would extend rather than replace.

---

## Smaller loose ends

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
