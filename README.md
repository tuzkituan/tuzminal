# Tuzminal

A fast, modular, GPU-accelerated terminal emulator with tabs, split panes,
installable themes, and a plugin system that runs both Lua and WebAssembly.

Built in Rust on `winit` + `wgpu`. Linux/Wayland first, with the platform layer
abstracted so macOS and Windows are a port rather than a rewrite.

> **Status: M1–M5 complete.** It runs shells, renders text, splits, tabs with a
> drawn tab bar, themes, a plugin status bar, and loads Lua and WASM plugins.
> 463 tests pass, including GPU tests that read pixels back. macOS and Windows
> are untested. See [Status](#status) for the honest detail.

## Build and run

Requires Rust 1.85+ and, on Linux, the Wayland/X11 and fontconfig development
packages.

```bash
# Fedora
sudo dnf install wayland-devel libxkbcommon-devel fontconfig-devel libX11-devel

cargo build --release
./target/release/tuzminal
```

## Using it

```bash
tuzminal --init-config     # write a commented starter config
tuzminal --config-check    # validate config, theme, keybindings and plugins
tuzminal --list-keys       # the resolved keymap, including plugin bindings
tuzminal --list-actions    # everything bindable
tuzminal --list-themes
tuzminal -v                # debug logging (-vv trace)
```

Default keys — all `ctrl+shift`, because plain `ctrl+<key>` belongs to the program
running inside the terminal:

| Key | Action |
|---|---|
| `ctrl+shift+d` / `e` | split right / down |
| `ctrl+shift+h j k l` or arrows | move focus |
| `ctrl+shift+w` | close pane |
| `ctrl+shift+t`, `ctrl+tab` | new tab, next tab |
| `ctrl+shift+c` / `v` | copy / paste |
| `ctrl+shift+plus` / `minus` / `0` | font size |
| `shift+pageup` / `pagedown` | scroll |
| `ctrl+shift+r` | reload config |

Config lives at `$XDG_CONFIG_HOME/tuzminal/config.toml`. Every setting is
optional, and **saving the file applies changes immediately**.

```toml
theme = "tuz-dark"

[font]
family = "JetBrains Mono"
size = 13.0
ligatures = true

[window]
padding = { x = 10, y = 8 }
opacity = 0.95
always_show_tab_bar = false   # the bar hides itself with a single tab

[keys]
"ctrl+shift+enter" = "split_right"
"ctrl+shift+t" = "none"          # remove a default binding
```

Keybindings **layer over the defaults** rather than replacing them, so adding one
chord does not cost you the other twenty-two. Bind to `"none"` to remove one.

## Packages

```bash
tuzminal registry update                    # clone/pull the registry index
tuzminal plugin search bar
tuzminal plugin install statusbar           # by registry name
tuzminal plugin install https://host/u/repo # or straight from git
tuzminal plugin list | remove <name> | update [name]
tuzminal theme  install | list | remove | update
```

Installing a plugin **shows the permissions it asks for and requires
confirmation** before writing anything, and says plainly when a plugin is not
sandboxed. A package manager that installed silently would make the permission
system decorative.

## Plugins

A plugin is a directory with a `plugin.toml` and an entry file.

```toml
name = "greeter"
version = "0.1.0"
api_version = 1
runtime = "lua"          # or "wasm"
entry = "init.lua"
permissions = []         # e.g. ["read-output", { fs-read = "~/notes" }]
```

```lua
local M = {}
function M.on_startup(ctx)
  ctx.register_command("hello", "Say hello")
  ctx.register_keybind("ctrl+shift+g", "hello")
end
function M.on_command(ctx, e)
  if e.name == "hello" then ctx.send_text("echo hello\n") end
end
function M.on_key(ctx, key)
  if key.chord == "ctrl+shift+q" then return true end  -- claim the key
end
return M
```

**The two runtimes are not equally safe, and Tuzminal says so.** WASM permissions
are structural — an ungranted host function is never linked into the instance, so
it cannot be called. Lua permissions are a restricted global environment: they
prevent accidents and document intent, but a determined plugin can get around
them. Installing a Lua plugin means trusting its code, and both the installer and
the startup log tell you that.

Every callback runs under a hard budget — an instruction hook for Lua, engine fuel
for WASM. A plugin that loops forever loses one aborted call, not your terminal,
and one that keeps failing is disabled.

## Design notes

**A broken config never breaks a running terminal.** Startup falls back to
built-in defaults; a live edit that fails to parse keeps the previous good
settings and surfaces the error. One bad keybinding costs that binding, not the
keymap. Validation reports every problem at once.

**Reloads do the minimum work.** `Config::diff` computes exactly what changed —
recolor on a theme switch, recompute geometry on a padding change, re-rasterize
the glyph atlas only when the font stack actually moves. Settings that cannot be
applied to a running process are reported as "restart to apply", never silently
ignored.

**Plugins never touch renderer or terminal state.** They get read-only event
snapshots and emit commands onto a queue the main thread drains. That boundary is
why one versioned API serves both runtimes and why a plugin can be aborted
mid-callback without leaving anything half-mutated.

**One draw call.** Cell backgrounds, glyphs, underlines, the cursor, split
dividers, the tab bar and the status bar are all instanced quads in a single
buffer with one pipeline. Glyphs are cached as white coverage and tinted in the
shader, so one bitmap serves every color the character appears in.

**Font fallback searches every installed font, not a configured list.** When the
primary font and the user's fallbacks all lack a character, the whole font database
is scanned for one that has it, and the answer is cached — misses included, or a
single unmappable character would rescan every font on every frame. Without this,
prompt symbols like `⑂` and `◈` render as blank cells while every other terminal on
the machine draws them.

**Chrome takes its colors from the theme.** The active tab uses the focused pane
background so the strip reads as continuous with the terminal below it; the tab
bar's height comes from the font metrics, not a fixed pixel value, so it scales
with the text. The bar hides itself with a single tab unless
`always_show_tab_bar` is set — a permanent strip costs a row of terminal to convey
nothing.

## Workspace

| Crate | Responsibility | Tests |
|---|---|---|
| `tuz-config` | TOML schema, themes, live reload, reload diffing | 57 |
| `tuz-core` | PTY sessions, VT state, color resolution, key encoding | 78 |
| `tuz-font` | Discovery, system-wide fallback, shaping, glyph atlas | 42 |
| `tuz-input` | Keychord grammar, actions, keymap | 39 |
| `tuz-layout` | BSP split tree, tabs, chrome strips, geometric focus | 73 |
| `tuz-plugin` | Host, Lua runtime, WASM runtime | 57 |
| `tuz-plugin-api` | Event/command/manifest contract | 14 |
| `tuz-render` | Instanced wgpu renderer, text layout, tab/status bars | 48 |
| `tuzminal` | Application, GPU surface, CLI, package manager | 54 |

`tuz-core` wraps `alacritty_terminal`, which supplies a battle-tested VT500
implementation *and* a cross-platform PTY (openpty on unix, ConPTY on Windows)
with its own I/O thread. Using all three instead of `portable-pty` removed a whole
layer and means most of the Windows port already exists.

## Status

**Working and verified on Linux/Wayland:** shells run; text, colors, bold/italic,
all five underline styles, strikethrough, CJK, combining marks and true color
render; splits and tabs with drag-resizable dividers; a tab bar with titles from
OSC 0/2, an active-tab marker, activity dots and click-to-switch; a status bar fed
by plugins; mouse selection and clipboard; scrollback; SGR mouse reporting;
bracketed paste; live config reload; Lua and WASM plugins; the package manager CLI.

**Not done:** subpixel antialiasing; IME for CJK input; `select_all`; plugin
config overlays; tab reordering and a close button on tabs. macOS and Windows
compile-target support is in place via the abstractions but **has never been built
or run** — treat it as unverified.

## Development

```bash
cargo test --workspace --features tuz-core/test-util   # 463 tests
cargo clippy --workspace --all-targets                 # clean
cargo fmt --all
```

The pure-logic crates carry most of the tests, because split geometry, chord
parsing, config diffing and VT encoding are where subtle bugs hide and where they
are cheapest to catch. `crates/tuzminal/tests/render.rs` goes further and renders
to an offscreen GPU texture, reading pixels back to prove text, splits and the tab
bar actually reach the framebuffer — that suite caught a metrics bug that produced
a 9600×20112 cell while every unit test still passed.

Two testing rules learned the hard way here, both now enforced:

- **No silent skips.** Helpers that returned `Option`/`Result` and let tests
  `return` early meant whole modules reported success without running. They panic
  with an actionable message instead.
- **Assert absolute bounds, not just relations.** `height > width` was satisfied
  by a cell 500× too large. The assertions now pin values against the font size.

## License

MIT OR Apache-2.0
