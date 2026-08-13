# Build log

A record of how Tuzminal was built, what was decided, and what went wrong. Not a
changelog — the code says what it does. This is for the things a future reader cannot
reconstruct from the source: why a dependency was dropped, why a design was rejected,
and which bugs cost the most time.

---

## What exists

Nine crates. The split is not decorative: the pure-logic crates carry most of the
tests, because split geometry, chord parsing, config diffing and VT encoding are
where subtle bugs hide and where tests are cheapest.

| Crate | Owns |
|---|---|
| `tuz-config` | TOML schema, theme resolution, live reload, reload diffing |
| `tuz-core` | PTY sessions, VT state, color resolution, key encoding, render snapshots |
| `tuz-font` | Font discovery and fallback, shaping, rasterization, glyph atlas |
| `tuz-input` | Keychord grammar, actions, keymap resolution |
| `tuz-layout` | BSP split tree, tabs, chrome strips, geometric focus |
| `tuz-plugin` | Plugin host, Lua runtime, WASM runtime |
| `tuz-plugin-api` | The event/command/manifest contract |
| `tuz-render` | Instanced wgpu renderer, text layout, tab and status bars |
| `tuzminal` | Application, GPU surface, CLI, package manager |

Four decisions shape everything else:

**A broken config never breaks a running terminal.** Startup falls back to built-in
defaults; a live edit that fails to parse keeps the previous good settings and
surfaces the error. One bad keybinding costs that binding, not the keymap.
Validation reports every problem at once rather than one per attempt.

**Reloads do the minimum work.** `Config::diff` decides what actually changed —
recolor on a theme switch, recompute geometry on a padding change, re-rasterize the
atlas only when the font stack moves. Settings that cannot apply to a running
process are reported as "restart to apply", never silently ignored.

**Plugins never touch renderer or terminal state.** They receive read-only event
snapshots and emit commands onto a queue the main thread drains. That boundary is
why one versioned API serves two runtimes, and why a misbehaving plugin can be
aborted mid-callback without leaving anything half-mutated.

**One draw call.** Cell backgrounds, glyphs, decorations, the cursor, split
dividers, the tab bar and the status bar are all instanced quads in a single buffer
drawn by one pipeline. Glyphs are cached as white coverage and tinted in the shader,
so one cached bitmap serves every color a character ever appears in.

---

## Deviations from the original plan

Each of these was a deliberate change made after learning something, not drift.

### `portable-pty` was dropped

The plan called for a `tuz-pty` crate wrapping `portable-pty`. On inspection,
`alacritty_terminal` 0.26 already ships a cross-platform PTY — `openpty` on unix,
ConPTY on Windows — **and** the I/O thread that pumps it, with a `Notifier` channel
for writes. Layering another PTY crate on top would have been a redundant
passthrough.

Using what was already there removed an entire crate and means most of the Windows
port already exists. The cost is a dependency on a semi-internal API, which is why
`tuz-core` confines it behind our own types: everything downstream sees
`RenderCell` and `TerminalFrame`, not `alacritty_terminal`.

### Plugins run on the UI thread, not their own

The plan said "plugin host thread". That turned out to buy nothing and cost a lot.

`on_key` needs a *synchronous* answer — the terminal cannot decide what to do with a
keystroke until it knows whether a plugin claimed it. An off-thread host would need
a request/response handshake with a deadline on every single keystroke, and the
deadline is the only thing actually protecting the frame. Running inline with a hard
per-callback budget gives the same protection with none of the cross-thread
machinery, and it lets the Lua runtime use mlua's cheaper single-threaded build.

The trait is deliberately not `Send`, with that reasoning recorded at the definition.

### WASM uses a core-module ABI, not the Component Model

The plan specified WIT and `wasmtime::component::bindgen!`. The implementation uses
a plain core-module ABI with JSON messages in linear memory instead.

The reason is toolchain weight. With the Component Model, writing a plugin means
`wit-bindgen`, a component build step, and adapters. With a core module, anything
that can emit WASM works — including hand-written `.wat`, which is what the tests
use, so the real engine, real ABI and real fuel limit are exercised with no
cross-compiler in the test environment.

Permissions are still structural: an ungranted host function is never linked into the
instance, so a plugin importing something it was not granted fails to instantiate.
That property is what matters, and it survives the change.

---

## Bugs worth remembering

### Font metrics were scaled by font units, not pixels

`swash` has both `Metrics::scale(ppem)` and `Metrics::linear_scale(factor)`. The
first divides by units-per-em; the second does not. Using `linear_scale(size_px)`
produced metrics roughly a thousand times too large: **a 9600×20112 cell**.

The consequences cascaded. The window divided by that cell size yielded a 1×1 grid,
and a single-column grid panics inside `alacritty_terminal` — the cursor is allowed
to advance to column 1, which is then out of bounds. The visible symptom was
`index out of bounds: the len is 1 but the index is 1` from a PTY thread, several
layers away from the actual mistake.

Two fixes: use `scale(ppem)`, and clamp `TermSize` to a minimum of two columns so
the degenerate grid is unreachable even from a tiny window.

### `fontdb` does not resolve fontconfig aliases

`family = "monospace"` matched nothing. `fc-match monospace` resolves fine on the
same machine, but that is fontconfig doing alias resolution; `fontdb` only matches
real family names, and no font is literally called "monospace".

Now resolution has four tiers: the exact name, a list of known-good monospace
families, any installed family whose name contains "mono", and finally any face at
all with a loud warning that text will not align.

### Capital letters were impossible to type

The key handler builds a *normalized* chord for keymap lookup: `D` becomes shift plus
`d`, so a binding written `ctrl+shift+d` can be matched. It then encoded the PTY
bytes from that same normalized chord — so every capital arrived lowercase.

Encoding now uses the raw key. For plain typing it prefers the platform's composed
text, which also fixed something unnoticed: dead-key compositions like `´` then `e`
were being dropped entirely, because the single-character path rejects multi-char
input.

The decision is extracted into `keys::bytes_for_key` specifically so it can be
tested without a window. The previous version looked correct at every individual call
site, which is exactly why it survived.

### Prompt symbols rendered as blank cells

A shell prompt drawing `⑂` (U+2442), `◈` (U+25C8), `▰` (U+25B0) and `❯` (U+276F)
showed gaps, while GNOME Terminal rendered them fine on the same machine.

Not a Nerd Font problem — these are ordinary BMP symbols. Font fallback searched only
the primary font, the user's configured fallback list, and the regular face.
Characters outside all three were silently dropped.

A fourth tier now scans the entire font database for a face covering the character,
loads it, and caches the answer. **Misses are cached too**: without that, one
unmappable character rescans every installed font on every frame it is visible. ASCII
never reaches the scan, so the hot path is unchanged.

A note on finding this: the first test written for it asserted that U+E0A0 (the
powerline branch glyph) would need the system scan. It failed — Source Code Pro
already has that codepoint. That failure was useful, because it proved the assumption
about *which* codepoint was broken was wrong and forced reading the actual shell theme
instead of guessing. The tests now assert against the exact symbol set the theme uses.

---

## Two testing lessons, both now enforced

### Silent skips made whole modules report success without running

Test helpers returned `Option`/`Result`, and every test began:

```rust
let Some(sys) = system() else { return };
```

When font family resolution broke, `system()` returned `None` every time and the
entire `tuz-font` and `tuz-render` suites reported **ok** while executing nothing.
The metrics bug above lived behind that for several commits.

Helpers now panic with an actionable message instead:

```rust
FontSystem::new(&config(), 1.0).expect(
    "no usable system font: install a monospace font (e.g. DejaVu Sans Mono) to run these tests",
)
```

A machine with no fonts is a real failure worth seeing, not a reason to pass.

### Relational assertions can be satisfied by absurd values

The cell-metrics test asserted:

```rust
assert!(m.height > m.width);
```

A 9600×20112 cell satisfies that perfectly. The assertion now pins absolute bounds
against the font size, with the reason recorded in the test:

```rust
assert!((0.3 * px..=1.2 * px).contains(&(m.width as f32)));
assert!((0.8 * px..=2.5 * px).contains(&(m.height as f32)));
```

The general rule: an assertion about a *relationship* passes for whole families of
wrong values. Where a real bound exists, assert the bound.

### What the GPU tests are for

`crates/tuzminal/tests/render.rs` renders to an offscreen texture and reads the
pixels back. Unit tests can prove the right instances were generated; only reading
pixels proves they were drawn. That suite is what catches a wrong vertex layout, an
inverted UV, a bad clip-space transform, or a shader sampling nothing — every one of
which produces a plausible-looking instance buffer and a blank window.

---

## What is not done

- **Subpixel antialiasing.** The renderer always uses grayscale coverage. Real
  subpixel AA needs three-channel coverage in the atlas and a dual-source blend.
  `grayscale_antialiasing` stays in the schema so existing configs keep parsing, and
  is documented as having no effect yet.
- **IME.** Wayland `text-input-v3` for CJK input is genuinely involved and is not a
  small addition.
- **`select_all`**, and plugin config overlays (`SetConfigOverlay` is accepted and
  logged, not applied).
- **macOS and Windows.** The platform layer is abstracted and `alacritty_terminal`
  supplies ConPTY, but neither target has ever been built or run. Treat as
  unverified rather than done.

---

## Design notes: the UI layer

Recorded here rather than only in a chat log, since it explains choices that look
arbitrary in the code.

**Widget logic is a separate crate (`tuz-ui`), and immediate mode.** The widget list
is rebuilt every frame from the current config, so there is no retained tree to keep
in sync — the class of bug where the panel displays a stale value cannot occur.
Focus order, hit-testing and value clamping are pure functions, which is where tests
are cheap; drawing stays in `tuz-render` and state stays in `tuzminal`.

**No text input field, deliberately.** The font family is a dropdown over installed
monospace families rather than a text box. Building a text field means a cursor,
selection, clipboard and eventually IME — a genuine rabbit hole for one setting. The
dropdown is less code *and* better: you cannot typo a family name and discover it at
the next restart.

**Settings apply live; saving is explicit.** Changing the theme or font size takes
effect immediately, because seeing the effect while choosing is the whole advantage
over editing TOML. `Save` writes to disk, `Revert` restores the config as it was when
the panel opened, and `Escape` closes while keeping unsaved changes for the session —
matching how the existing font-size keybindings already behave.

**Saving must not destroy the config file.** `toml::to_string` would rewrite
`config.toml` and delete every comment in it, and the file users start from is
heavily commented. Saving therefore uses `toml_edit` to set only changed keys in the
existing document, and writes atomically via temp-file-and-rename so an interrupted
save cannot truncate a working config. The most direct test in that area asserts that
comments and untouched lines survive byte-identical.

**Hover redraws only when the hovered widget changes.** Requesting a frame on every
`CursorMoved` would repaint continuously while the mouse crosses the window, which
would undo the whole point of the event-driven redraw policy.

**Window controls only when `decorations = false`.** With decorations on the
compositor already draws them, and drawing our own would put two sets side by side.
