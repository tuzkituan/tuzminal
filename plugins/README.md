# Plugins

Every plugin that ships with Tuzminal lives here, one directory each. They are ordinary
plugins — the same format anyone else writes, loaded through the same host, with no
privileged path of their own. What makes these special is only that they are
compiled into the binary and written out on first launch, so a fresh install has working
code to read rather than an empty page and a link to the docs.

| | |
|---|---|
| [`clock/`](clock) | A tour of the API: a status segment, a keybinding, a command, a config-reload hook |
| [`open-in-ide/`](open-in-ide) | Buttons in the status bar that open the working directory in VS Code, Cursor and friends |
| [`suggest/`](suggest) | Ghost-text completions from your shell history. The plugin `input_line` and `set_inline_hint` were added for |

**[../docs/PLUGINS.md](../docs/PLUGINS.md) is the guide to writing one.**

## Adding a plugin here

A plugin is a directory with a `plugin.toml` and an entry file:

```
plugins/my-plugin/
  plugin.toml
  init.lua
```

Dropping it in this directory is enough to develop against — point the terminal at it,
or copy it into `~/.local/share/tuzminal/plugins/`. Two extra steps make it *ship*:

1. Add it to `BUNDLED` in `crates/tuzminal/src/bundled.rs`, so it is written out on
   first launch. The `include_str!` paths there mean a manifest that does not parse, or
   an entry file that is named wrong, is a **compile** error rather than something every
   new user discovers at startup.
2. Add a test to `crates/tuz-plugin/tests/example_plugin.rs` if it exercises a part of
   the API nothing else covers. Those tests load from this directory, which is what
   keeps the shipped plugins working as the host changes underneath them.

Nothing here is loaded from the repository at runtime. The copies under
`~/.local/share/tuzminal/plugins/` are what the terminal runs, and a plugin you have
edited there is never overwritten — see the comment at the top of `bundled.rs`.
