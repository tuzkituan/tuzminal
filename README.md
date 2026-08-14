# Tuzminal

A fast, modular, GPU-accelerated terminal emulator: tabs, split panes, a file explorer,
history autosuggestions, installable themes, and a plugin system that runs both Lua and
WebAssembly.

![Tuzminal running `sudo dnf update`](docs/preview.png)

Built in Rust on `winit` + `wgpu`. Linux/Wayland first, with the platform layer abstracted
so macOS and Windows are a port rather than a rewrite.

> **Honest status.** It runs shells, splits, tabs, themes, plugins and a file browser, with
> 851 tests including GPU tests that read pixels back. **macOS and Windows compile but have
> never been run.** The working-directory features are Linux-only, because they read
> `/proc`. See [Status](#status).

---

## Install

Download a package from the [latest release](https://github.com/tuzkituan/tuzminal/releases).
**No Rust toolchain, no compiling.**

```bash
sudo dnf install ./tuzminal-0.1.0-1.x86_64.rpm     # Fedora, RHEL, openSUSE
sudo apt install ./tuzminal_0.1.0-1_amd64.deb      # Debian, Ubuntu, Mint, Pop!_OS
```

For any other Linux, a portable archive that installs for your user only and needs no root:

```bash
tar -xzf tuzminal-0.1.0-x86_64-linux.tar.gz
cd tuzminal-0.1.0-x86_64-linux && ./install.sh
```

The `.rpm` and `.deb` register the desktop entry for you; the archive puts the binary in
`~/.local/bin` and registers it there. Then launch it from your applications menu, or run
`tuzminal`.

Every release ships `SHA256SUMS.txt`, so a download can be checked with
`sha256sum -c SHA256SUMS.txt --ignore-missing`.

### Building from source instead

Only if you want to. Rust 1.85+ and the development headers:

```bash
sudo dnf install wayland-devel libxkbcommon-devel fontconfig-devel libX11-devel   # Fedora
sudo apt install libwayland-dev libxkbcommon-dev libfontconfig-1-dev libx11-dev   # Debian

git clone https://github.com/tuzkituan/tuzminal && cd tuzminal
cargo install --path crates/tuzminal --locked
tuzminal --install-desktop-entry    # add it to the applications list
```

### Making it your default, and removing it

```bash
gsettings set org.gnome.desktop.default-applications.terminal exec tuzminal   # GNOME

sudo dnf remove tuzminal    # or: apt remove tuzminal, ./uninstall.sh, cargo uninstall
```

Removing the package leaves your settings, themes and plugins alone. `rm -rf
~/.config/tuzminal ~/.local/share/tuzminal` removes those too, and the archive's
`./uninstall.sh --purge` does both. Nothing is written anywhere outside those two
directories, the binary, and the desktop entry and icon.

## Using it

```bash
tuzminal                   # start a shell
tuzminal --config-check    # validate config, theme, keybindings and plugins
tuzminal --list-keys       # the resolved keymap, including plugin bindings
tuzminal --list-themes     # or --list-actions
```

To run something other than your login shell, set `[shell] program` in the config —
there is no command-line flag for it.

### Keys

Press <kbd>F1</kbd> for the full list, generated from your live keymap rather than written
down — rebind something and the page says what you rebound it to.

| | |
|---|---|
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>t</kbd> | new tab |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>d</kbd> / <kbd>e</kbd> | split right / down |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>w</kbd> | close pane |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>hjkl</kbd> or arrows | move between panes |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>c</kbd> / <kbd>v</kbd> | copy / paste |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>b</kbd> | file explorer |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>p</kbd> | plugins |
| <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>,</kbd> | settings |
| <kbd>F1</kbd> | all shortcuts |

### The toolbar

`+` opens a tab and the chevron beside it picks which shell. `☰` holds settings, shortcuts
and plugins, and collects any button that did not fit. The split and file-explorer buttons
have their own icons. Window controls appear only when `decorations = false`, so they are
never drawn twice, and sit behind a divider so close is not flush against a panel button.

### Suggestions as you type

The bundled `suggest` plugin completes from your shell history in dim ghost text after the
cursor. <kbd>→</kbd> at end of line takes it, <kbd>alt</kbd>+<kbd>→</kbd> takes one word,
and it stays quiet inside full-screen programs and at password prompts. Turn it off on the
plugins page like anything else.

### The file explorer

<kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>b</kbd> opens a sidebar in the shell's current
directory. Arrows move, <kbd>enter</kbd> opens a folder or a file in `$EDITOR`,
<kbd>backspace</kbd> goes up, and <kbd>escape</kbd> hands the keyboard back to the shell
without closing it.

`p` types the selected path at the prompt, `c` runs `cd`, `e` opens in `$EDITOR`, and
`r` / `n` / `d` rename, create a folder and delete. Every path written into your shell is
quoted, so a file called `$(rm -rf ~)` is a filename and not a command.

---

## Configuration

```bash
tuzminal --init-config     # write a commented starter config
```

That creates `~/.config/tuzminal/config.toml`, heavily commented and listing every option.
Saving applies most changes immediately; anything needing a restart says so rather than
being silently ignored. The settings page — <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>,</kbd> —
writes the same file **and keeps your comments**.

## Themes

Ten are built in: `tuz-dark`, `tuz-light`, `catppuccin-mocha`, `dracula`, `gruvbox-dark`,
`nord`, `one-dark`, `solarized-dark`, `solarized-light` and `tokyo-night`. Pick one in the
settings page and watch it apply as you scroll, or:

```bash
tuzminal theme list | install <url-or-name> | remove <name> | update [name] | search <term>
```

## Plugins

**[docs/PLUGINS.md](docs/PLUGINS.md) is the guide.** Three plugins ship in
[`plugins/`](plugins) and are installed on first launch, so there is something to read and
something to toggle straight away: `clock` (a tour of the API), `open-in-ide` (status-bar
buttons), and `suggest` (history autosuggestions).

Manage them from the plugins page — <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>p</kbd> — which
lists what is on disk, enables and disables without a restart, and imports or exports
through your desktop's folder chooser. Or:

```bash
tuzminal plugin list | install <url-or-name> | remove <name> | update [name] | search <term>
```

Installing a plugin **shows the permissions it asks for and requires confirmation** before
writing anything, and says plainly that Lua plugins are not sandboxed.

---

## Troubleshooting

**It is not in my applications list, or the launcher does nothing.** Run `tuzminal
--install-desktop-entry`; the entry points at the binary's absolute path, so moving or
reinstalling it needs another run. Some desktops need a log out and back in.

**`tuzminal: command not found`.** `~/.local/bin` (archive) or `~/.cargo/bin` (source) is
not on your `PATH`. Add it to your shell profile.

**Blank squares instead of prompt symbols.** Install a Nerd Font and set it in
`config.toml`. Tuzminal searches every installed font for missing characters, so a blank
cell means no font on the machine has that glyph.

**It will not start.** `tuzminal --config-check` validates config, theme, keybindings and
plugins without opening a window, and reports every problem at once.

**A plugin is misbehaving.** Turn it off on the plugins page, or run `RUST_LOG=debug
tuzminal` to see what it is doing. A plugin that fails three calls in a row is disabled
automatically for the session.

---

## Design notes

The decisions worth knowing, with the full reasoning — including what was tried and
abandoned — in [docs/BUILD-LOG.md](docs/BUILD-LOG.md).

**A broken config never breaks a running terminal.** Startup falls back to defaults; a live
edit that fails to parse keeps the last good settings and surfaces the error. One bad
keybinding costs that binding, not the keymap.

**Reloads do the minimum work.** `Config::diff` computes exactly what changed, down to
re-rasterizing the atlas only when the font stack really moves. What cannot apply to a
running process says "restart to apply" rather than being silently dropped.

**Plugins never touch renderer or terminal state.** They get read-only event snapshots and
emit commands onto a queue the main thread drains — which is why one versioned API serves
both runtimes, and why a plugin can be aborted mid-callback without leaving anything
half-mutated.

**One draw call.** Cells, glyphs, underlines, the cursor, dividers, the tab bar and the
status bar are all instanced quads in one buffer with one pipeline. Glyphs are cached as
white coverage and tinted in the shader, so one bitmap serves every color it appears in.
Ghost text is the same mechanism: extra cells appended to the frame, no second pass.

**Font fallback searches every installed font, not a configured list**, and caches the
answer — misses included, or one unmappable character would rescan every font every frame.
Without it, prompt symbols like `⑂` and `◈` are blank cells while every other terminal draws
them.

**Chrome colors are derived from the theme, never picked from its palette.** The active tab
is painted in the pane background and runs into it, so the join is seamless. Secondary text
is the foreground blended toward the background, because "dim" is a *relationship* to the
background and not a color — a slot that reads as quiet on a dark theme is invisible on a
light one. That was gotten wrong twice; a test now asserts legibility across every bundled
theme.

## Workspace

| Crate | Responsibility | Tests |
|---|---|---|
| `tuz-config` | TOML schema, themes, live reload, diffing, saving | 92 |
| `tuz-core` | PTY sessions, VT state, color resolution, key encoding | 105 |
| `tuz-font` | Discovery, system-wide fallback, shaping, glyph atlas | 42 |
| `tuz-input` | Keychord grammar, actions, keymap | 38 |
| `tuz-layout` | BSP split tree, tabs, chrome strips, geometric focus | 101 |
| `tuz-plugin` | Host, Lua runtime, WASM runtime, shipped-plugin tests | 83 |
| `tuz-plugin-api` | Event/command/manifest contract | 17 |
| `tuz-render` | Instanced wgpu renderer, text layout, chrome, widgets | 86 |
| `tuz-ui` | Widget model, focus order, hit-testing, scrolling | 86 |
| `tuzminal` | Application, GPU surface, CLI, packages, and the settings, plugins, shortcut and file-explorer pages | 197 |

`tuz-core` wraps `alacritty_terminal`, which supplies a battle-tested VT500 implementation
*and* a cross-platform PTY (openpty on unix, ConPTY on Windows) with its own I/O thread.
Using all three instead of `portable-pty` removed a whole layer and means most of the Windows
port already exists.

## Status

Working: shells, splits, tabs, the file explorer, themes, plugins, history autosuggestions,
the settings, shortcuts and plugins pages, mouse selection and clipboard, scrollback, SGR
mouse reporting, bracketed paste, live config reload, and the package manager.

Not done, and honestly so:

- **macOS and Windows compile but have never been run.** Treat them as unported.
- The working directory shown in the status bar and used by the explorer is read from
  `/proc`, so it is Linux-only. Elsewhere those features degrade rather than fail.
- No subpixel antialiasing, and no IME, so composed input is not supported.
- No shell integration (OSC 133), so autosuggestions work out where your prompt ends by
  watching the line rather than being told. See [docs/PLUGINS.md](docs/PLUGINS.md).
- Plugins cannot draw, apart from status segments and one line of ghost text at the cursor.
  They can bind keys, register commands and drive panes — see
  [docs/PLUGINS.md](docs/PLUGINS.md) for the boundary and why it is there.
- Settings, shortcuts and plugins are whole tabs, so they cannot share a tab with a split or
  the file explorer. See `docs/NEXT-STEPS.md`.

## Development

```bash
cargo test --workspace --features tuz-core/test-util   # 851 tests
cargo clippy --workspace --all-targets                 # clean; CI runs with -D warnings
cargo fmt --all
cargo bench --workspace --features tuz-core/test-util  # VT parser throughput
./scripts/package.sh                                   # .rpm, .deb and .tar.gz in dist/
```

`package.sh` strips the binary first — the release profile keeps debug symbols so perf
profiles stay readable, and a download should not carry them. The `.rpm` and `.deb` need
`cargo install cargo-deb cargo-generate-rpm`, and are skipped with a note if absent rather
than failing the build. CI builds and tests all three platforms on every push, plus clippy,
rustfmt and `cargo audit`.

**Package dependencies are listed by hand, not detected.** Every windowing library is
`dlopen`ed by `winit` and `wgpu` at runtime rather than linked, so the binary's ELF header
names only libc, libm and libgcc — `dpkg-shlibdeps` and `auto-req` both find nothing, and an
auto-generated package would install happily onto a system it cannot run on. Vulkan is a
*recommendation*, not a requirement, because there is a working GL backend behind
`performance.gpu_backend = "gl"`.

The pure-logic crates carry most of the tests: split geometry, chord parsing, config diffing
and VT encoding are where subtle bugs hide and where they are cheapest to catch.
`crates/tuzminal/tests/render.rs` goes further and renders to an offscreen GPU texture,
reading pixels back — that suite caught a metrics bug producing a 9600×20112 cell while every
unit test still passed. Hence two rules, both now enforced:

- **No silent skips.** Helpers returning `Option`/`Result` that let tests `return` early meant
  whole modules reported success without running. They panic with an actionable message.
- **Assert absolute bounds, not just relations.** `height > width` was satisfied by a cell
  500× too large. Assertions now pin values against the font size.

## License

MIT OR Apache-2.0
