# Tuzminal

A fast, modular, GPU-accelerated terminal emulator with tabs, split panes, a file
explorer, installable themes, and a plugin system that runs both Lua and WebAssembly.

![Tuzminal running `sudo dnf update`](docs/preview.png)

Built in Rust on `winit` + `wgpu`. Linux/Wayland first, with the platform layer
abstracted so macOS and Windows are a port rather than a rewrite.

> **Honest status.** It runs shells, splits, tabs, themes, plugins and a file
> browser, with 757 tests including GPU tests that read pixels back. **macOS and
> Windows compile but have never been run.** The working-directory features are
> Linux-only, because they read `/proc`. See [Status](#status).

---

## Install

Download a package from the [latest release](https://github.com/tuzkituan/tuzminal/releases)
and install it. **No Rust toolchain, no compiling.**

### Fedora, RHEL, openSUSE

```bash
sudo dnf install ./tuzminal-0.1.0-1.x86_64.rpm
```

### Debian, Ubuntu, Mint, Pop!_OS

```bash
sudo apt install ./tuzminal_0.1.0-1_amd64.deb
```

### Any other Linux

A portable archive that installs for your user only, needing no root:

```bash
tar -xzf tuzminal-0.1.0-x86_64-linux.tar.gz
cd tuzminal-0.1.0-x86_64-linux
./install.sh
```

Then launch it from your applications menu, or run `tuzminal`.

The `.rpm` and `.deb` register the desktop entry for you. The archive's
`install.sh` puts the binary in `~/.local/bin` and registers it there; if your
shell cannot find `tuzminal` afterwards, that directory is not on your `PATH`.

### Verifying a download

Every release ships `SHA256SUMS.txt`:

```bash
sha256sum -c SHA256SUMS.txt --ignore-missing
```

### Building from source instead

Only if you want to. You need Rust 1.85+ and the development headers:

```bash
# Fedora
sudo dnf install wayland-devel libxkbcommon-devel fontconfig-devel libX11-devel
# Debian / Ubuntu
sudo apt install libwayland-dev libxkbcommon-dev libfontconfig-1-dev libx11-dev

git clone https://github.com/tuzkituan/tuzminal
cd tuzminal
cargo install --path crates/tuzminal --locked
tuzminal --install-desktop-entry    # add it to the applications list
```

### Set it as your default terminal

GNOME:

```bash
gsettings set org.gnome.desktop.default-applications.terminal exec tuzminal
```

## Uninstall

```bash
sudo dnf remove tuzminal          # Fedora
sudo apt remove tuzminal          # Debian / Ubuntu
./uninstall.sh                    # if you used the archive
cargo uninstall tuzminal          # if you built from source
```

Removing the package leaves your settings, themes and plugins alone. To remove
those too:

```bash
rm -rf ~/.config/tuzminal         # config, your themes, your plugins
rm -rf ~/.local/share/tuzminal    # installed themes and plugins
```

The archive's uninstaller does both with `./uninstall.sh --purge`. Nothing is
written anywhere outside those two directories, the binary, and the desktop entry
and icon.

## Using it

```bash
tuzminal                   # start a shell
tuzminal -e htop           # run a command instead
tuzminal --config-check    # validate config, theme, keybindings and plugins
tuzminal --list-keys       # the resolved keymap, including plugin bindings
```

### Keys

Press <kbd>F1</kbd> for the full list, generated from your live keymap rather than
written down — rebind something and the page says what you rebound it to.

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

`+` opens a tab and the chevron beside it picks which shell. `☰` holds settings,
shortcuts and plugins. The split buttons and the file explorer have their own
icons, and the window controls appear only when `decorations = false`, so they are
never drawn twice.

### The file explorer

<kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>b</kbd> opens a sidebar in the shell's
current directory. Arrows move, <kbd>enter</kbd> opens a folder or a file in
`$EDITOR`, <kbd>backspace</kbd> goes up, and <kbd>escape</kbd> hands the keyboard
back to the shell without closing it.

`p` types the selected path at the prompt, `c` runs `cd`, `e` opens in `$EDITOR`,
and `r` / `n` / `d` rename, create a folder and delete. Every path written into
your shell is quoted, so a file called `$(rm -rf ~)` is a filename and not a
command.

---

## Configuration

```bash
tuzminal --init-config     # write a commented starter config
```

That creates `~/.config/tuzminal/config.toml`, which is heavily commented and lists
every option. Saving it applies most changes immediately; anything needing a restart
says so rather than being silently ignored. Or use the settings page —
<kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>,</kbd> — which writes the same file **and
keeps your comments**.

## Themes

Ten are built in: `tuz-dark`, `tuz-light`, `catppuccin-mocha`, `dracula`,
`gruvbox-dark`, `nord`, `one-dark`, `solarized-dark`, `solarized-light` and
`tokyo-night`.

```bash
tuzminal theme list
tuzminal theme install https://host/user/some-theme
```

Or pick one in the settings page and watch it apply as you scroll.

## Plugins

**[docs/PLUGINS.md](docs/PLUGINS.md) is the guide.** Two working examples ship in
[`examples/`](examples) and are installed on first launch, so there is something to
read and something to toggle straight away.

Manage them from the plugins page — <kbd>ctrl</kbd>+<kbd>shift</kbd>+<kbd>p</kbd> —
which lists what is on disk, enables and disables without a restart, and imports or
exports through your desktop's folder chooser. Or from the command line:

```bash
tuzminal plugin install https://host/user/repo
tuzminal plugin list | remove <name> | update [name]
```

Installing a plugin **shows the permissions it asks for and requires confirmation**
before writing anything, and says plainly that Lua plugins are not sandboxed.

---

## Troubleshooting

**It is not in my applications list.** The `.rpm` and `.deb` register it for you; if
you built from source or the entry went missing, run `tuzminal
--install-desktop-entry`. Some desktops need a log out and back in.

**The launcher does nothing.** The entry points at the binary's absolute path; if
you moved or reinstalled it, run `--install-desktop-entry` again.

**`tuzminal: command not found`.** If you used the archive, `~/.local/bin` is not on
your `PATH`; if you built from source, `~/.cargo/bin` is not (`source ~/.cargo/env`
fixes the current shell). Add the directory to your shell profile either way.

**Blank squares instead of prompt symbols.** Install a Nerd Font and set it in
`config.toml`. Tuzminal searches every installed font for missing characters, so a
blank cell means no font on the machine has that glyph.

**It will not start.** `tuzminal --config-check` validates your config, theme,
keybindings and plugins without opening a window, and reports every problem at once.

**A plugin is misbehaving.** Turn it off on the plugins page, or run with
`RUST_LOG=debug tuzminal` to see what it is doing. A plugin that fails three calls
in a row is disabled automatically for the session.

---

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
with the text. The strip is always shown by default, because it carries the
toolbar buttons as well as the tabs; `always_show_tab_bar = false` hides it with a
single tab open, and hides those buttons with it.

## Workspace

| Crate | Responsibility | Tests |
|---|---|---|
| `tuz-config` | TOML schema, themes, live reload, diffing, saving | 86 |
| `tuz-core` | PTY sessions, VT state, color resolution, key encoding | 84 |
| `tuz-font` | Discovery, system-wide fallback, shaping, glyph atlas | 42 |
| `tuz-input` | Keychord grammar, actions, keymap | 38 |
| `tuz-layout` | BSP split tree, tabs, chrome strips, geometric focus | 88 |
| `tuz-plugin` | Host, Lua runtime, WASM runtime | 65 |
| `tuz-plugin-api` | Event/command/manifest contract | 14 |
| `tuz-render` | Instanced wgpu renderer, text layout, chrome, widgets | 80 |
| `tuz-ui` | Widget model, focus order, hit-testing, scrolling | 81 |
| `tuzminal` | Application, GPU surface, CLI, packages, and the settings, plugins, shortcut and file-explorer pages | 175 |

`tuz-core` wraps `alacritty_terminal`, which supplies a battle-tested VT500
implementation *and* a cross-platform PTY (openpty on unix, ConPTY on Windows)
with its own I/O thread. Using all three instead of `portable-pty` removed a whole
layer and means most of the Windows port already exists.

## Status

Working: shells, splits, tabs, the file explorer, themes, plugins, the settings,
shortcuts and plugins pages, mouse selection and clipboard, scrollback, SGR mouse
reporting, bracketed paste, live config reload, and the package manager.

Not done, and honestly so:

- **macOS and Windows compile but have never been run.** Treat them as unported.
- The working directory shown in the status bar and used by the explorer is read
  from `/proc`, so it is Linux-only. Elsewhere those features degrade rather than
  fail.
- No subpixel antialiasing, and no IME, so composed input is not supported.
- Plugins cannot draw. They can add status segments, bind keys, register commands
  and drive panes — see [docs/PLUGINS.md](docs/PLUGINS.md) for the boundary and
  why it is there.

## Development

```bash
cargo test --workspace --features tuz-core/test-util   # 757 tests
cargo clippy --workspace --all-targets                 # clean
cargo fmt --all
cargo bench --workspace --features tuz-core/test-util   # VT parser throughput
./scripts/package.sh                                    # a release archive in dist/
```

`package.sh` produces all three artifacts in `dist/`: an `.rpm`, a `.deb`, and a
portable `.tar.gz`. It strips the binary first, which takes it from 167 MB to
25 MB — the release profile keeps debug symbols on purpose so perf profiles stay
readable, and a download should not carry them.

The `.rpm` and `.deb` need two cargo plugins, and are skipped with a note if absent
rather than failing the build:

```bash
cargo install cargo-deb cargo-generate-rpm
```

**Package dependencies are listed by hand, not detected.** Every windowing library
is `dlopen`ed by `winit` and `wgpu` at runtime rather than linked, so the binary's
ELF header names only libc, libm and libgcc — `dpkg-shlibdeps` and `auto-req` both
find nothing, and an auto-generated package would install happily onto a system it
cannot run on. Vulkan is a *recommendation* rather than a requirement, because
there is a working GL backend behind `performance.gpu_backend = "gl"`.

CI builds and tests Linux, macOS and Windows on every push, plus clippy, rustfmt and
`cargo audit`.

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
