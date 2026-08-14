# Writing a Tuzminal plugin

A plugin is a folder with a `plugin.toml` and one entry file. It can add status bar
segments, bind keys, register commands, drive panes and tabs, and type into your
shell. It cannot draw — see [What plugins cannot do](#what-plugins-cannot-do), which
is a deliberate boundary rather than a gap.

Two working plugins ship in [`plugins/`](../plugins): `clock`, a tour of the API in
forty lines, and `open-in-ide`, which was a built-in feature until it became a plugin.
Both are loaded by the test suite, so they are known to work against the current API
rather than being snippets that rotted.

`open-in-ide` is the more interesting one to read. Porting it is what added clickable
status segments — if the API cannot express something that ordinary, it is not an API
worth having.

---

## The shortest possible plugin

```
my-plugin/
  plugin.toml
  init.lua
```

**`plugin.toml`**

```toml
name = "my-plugin"
version = "0.1.0"
api_version = 1
runtime = "lua"
entry = "init.lua"
```

**`init.lua`**

```lua
local M = {}

function M.on_startup(ctx)
  ctx.notify("hello", "info")
end

return M
```

That last `return M` is not optional and is the most common mistake — the entry file
must *return* its handler table. Forgetting it produces a specific error saying so.

## Installing it

Three ways, all equivalent:

| | |
|---|---|
| **Plugins page** | `ctrl+shift+p`, type the folder path under **Import** |
| **By hand** | copy the folder into `~/.config/tuzminal/plugins/` |
| **From git** | `tuzminal plugin install https://host/user/repo` |

The Plugins page also toggles plugins on and off, which takes effect immediately —
the host reloads rather than waiting for a restart. Toggling off and on discards the
plugin's state, which is the honest meaning of turning it off.

Two search paths, in order: `~/.config/tuzminal/plugins` then
`~/.local/share/tuzminal/plugins`. A plugin in the first shadows one of the same name
in the second, so you can override an installed plugin with your own copy.

---

## Handlers

Every handler is optional. Anything you do not define is never called.

| Handler | When | Payload |
|---|---|---|
| `on_startup(ctx)` | once, after loading | — |
| `on_config_reload(ctx)` | config changed on disk or by keybinding | — |
| `on_status_bar_render(ctx)` | every frame the bar is drawn | — |
| `on_status_segment_click(ctx, e)` | one of your segments was pressed | `id` |
| `on_key(ctx, key)` | before the terminal sees a key | `chord`, `ctrl`, `shift`, `alt`, `super` |
| `on_command(ctx, e)` | a command you registered ran | `name`, `args` |
| `on_pane_opened(ctx, e)` / `on_pane_closed(ctx, e)` | a shell started or ended | `pane` |
| `on_tab_switch(ctx, e)` | the visible tab changed | `index` |
| `on_title_change(ctx, e)` | a program set the window title | `pane`, `title` |
| `on_bell(ctx, e)` | a program rang the bell | `pane` |
| `on_pane_output(ctx, e)` | a pane produced output | `pane`, `text` |
| `on_osc(ctx, e)` | an OSC sequence the terminal ignored | `pane`, `code`, `payload` |

Register keybinds and commands in `on_startup`. The keymap is built immediately
afterwards, so a binding made there is live from the first keystroke.

**`on_status_bar_render` runs at your refresh rate.** A slow handler there is felt as
a slow terminal. Cache anything expensive and update it from a cheaper event.

**`on_key` runs before every keystroke reaches the shell**, and has a much tighter
budget than other handlers — 5 ms by default, against 250 ms. Return `true` to
swallow the key; return nothing to let it through, which is what you want almost
always.

### Opting in to expensive events

`pane_output` is not delivered unless you ask for it twice — once in `events`, once in
`permissions`:

```toml
events = ["pane_output"]
permissions = ["read-output"]
```

It is everything your shell prints, including anything you type that a program echoes.
The install prompt says so, because a plugin reading it is reading your session.

Leaving `events` empty means "the cheap ones", which is every event above except
`pane_output`.

---

## The `ctx` API

Everything except `ctx.log` queues a command that runs after your handler returns.
Nothing takes effect mid-handler, which is why a plugin can be aborted for running too
long without leaving anything half-done.

```lua
ctx.notify(message, "info"|"warn"|"error")
ctx.log(message)                          -- to the terminal's own log

ctx.register_command(name, description)   -- makes `myplugin.name` bindable
ctx.register_keybind(chord, command)

ctx.send_text(text)                       -- type into the focused pane
ctx.send_text_to(pane, text)              -- or a specific one

ctx.new_tab()
ctx.select_tab(index)
ctx.split("left"|"right"|"up"|"down")
ctx.focus("left"|"right"|"up"|"down")
ctx.focus_pane(pane)
ctx.close_pane()                          -- the focused one
ctx.close_pane_id(pane)
ctx.resize("left"|"right"|"up"|"down", 0.1)

ctx.set_status({
  { text = "…", foreground = "#rrggbb", background = "#rrggbb" },
  { text = "VS", id = "code" },   -- an `id` makes it clickable
})
ctx.set_config("[font]\nsize = 14.0\n")   -- a TOML fragment, overlaid
ctx.reload_config()
ctx.quit()
```

`set_status` replaces **your** segments only; it never affects another plugin's.

---

## Permissions

Declared in `plugin.toml`, shown to the user at install, denied unless listed.

| Permission | Effect |
|---|---|
| `read-output` | required for `pane_output` |
| `fs-read = "/path"` / `fs-write = "/path"` | Lua keeps `io` and `os.remove`/`os.rename` |
| `spawn-process` | Lua keeps `os.execute` |
| `clipboard`, `network` | **declared but not yet implemented** |

Be honest about the last row: `clipboard` and `network` can be requested and shown to
the user, and grant nothing today. For WASM plugins, **no** permission grants a host
function yet — WASM denies by default, so the effect is "cannot be done", not
"unrestricted".

**Lua is not sandboxed.** The environment is trimmed — `require`, `load`, `dofile`,
`debug` and `os.exit` are removed, and the fs/process tables are removed unless
requested — but that is a way of documenting intent, not a security boundary. A
determined Lua plugin can get around it. Installing one means trusting its code, and
the installer says so. WASM is a real sandbox: memory-isolated, with only the two
imports the host links.

---

## Runtime limits

| | Default | Why |
|---|---|---|
| Callback budget | 250 ms | An over-budget call is aborted, not the terminal |
| `on_key` budget | 5 ms | Every keystroke waits on it |
| Consecutive failures | 3 | Then the plugin is disabled for the session |

Lua is interrupted by an instruction-count hook; WASM runs on a fuel budget. Either
way a plugin that loops forever loses that one call.

---

## WASM plugins

Set `runtime = "wasm"` and `entry = "plugin.wasm"`. It is a **core module**, not a
Component — so `wasm32-unknown-unknown` is the whole toolchain, with no `wit-bindgen`.

Exports you must provide:

```
memory
tuz_alloc(len: i32) -> i32
tuz_on_event(ptr: i32, len: i32) -> i64   // high 32 bits == 1 swallows a key
```

Imports available, in module `tuz`:

```
tuz_emit(ptr: i32, len: i32)   // a JSON Command
tuz_log(ptr: i32, len: i32)
```

Events arrive as JSON in linear memory; commands go back the same way. The JSON is
the `Event` and `Command` enums, externally tagged as `{"type": "snake_case", …}`.

WASM plugins can emit every command. Lua could not, until recently — if you are
reading an older plugin that works around a missing `ctx` function, that gap is closed.

---

## What plugins cannot do

Recorded here because these are decisions, not omissions:

- **Draw anything.** No canvas, no widgets, no panels. The only pixels a plugin
  influences are status segment text and its two colours.
- **Add a toolbar button, a tab, a page, or a menu entry.** Those are closed enums in
  the binary.
- **Receive a click anywhere but a status segment.** Segments given an `id` are
  hit-tested and reported through `on_status_segment_click`; nothing else is.
- **Read their own `[config]` table.** It is parsed from the manifest and stored, and
  not yet passed to either runtime.

The reason for the first two is in [`BUILD-LOG.md`](BUILD-LOG.md): plugins receive
read-only event snapshots and emit commands onto a queue. That boundary is why one
versioned API serves both Lua and WASM, and why a plugin can be aborted mid-callback
without leaving the renderer half-mutated. Opening it up means changing that decision
deliberately — the shape it would take is a plugin submitting a declarative widget
tree the host draws, since `tuz_ui::Widget` is already plain serialisable data.

---

## Debugging

```
RUST_LOG=debug tuzminal          # ctx.log output, load errors, timeout warnings
tuzminal --config-check          # validates manifests without starting a terminal
tuzminal --list-keys             # the resolved keymap, plugin bindings included
```

A plugin that fails to load says why and does not stop the terminal starting. A
plugin that fails three calls in a row is disabled for the session, with a warning.
