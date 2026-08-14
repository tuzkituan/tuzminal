-- A tour of the plugin API in about forty lines.
--
-- Every handler is optional. Anything you do not define is simply never called,
-- so a plugin that only wants one event defines only that one.

local M = {}

-- Called once, after the plugin loads. Register anything you want bound here:
-- registrations made later still work, but the keymap is built right after this,
-- so doing it here means your binding is live from the first keystroke.
function M.on_startup(ctx)
  ctx.register_command("hello", "Type a greeting into the shell")
  ctx.register_keybind("ctrl+shift+m", "hello")
  ctx.log("clock plugin ready")
end

-- Called when a command you registered is invoked, whether from your own
-- keybind or from a `[keys]` entry the user wrote. The name arrives without the
-- `clock.` prefix the host adds.
function M.on_command(ctx, e)
  if e.name == "hello" then
    -- Typed at the prompt, not run: it is inserted as if pasted, so you get to
    -- see it before pressing Enter.
    ctx.send_text("echo hello from a plugin")
    ctx.notify("sent a greeting", "info")
  end
end

-- Called every frame the status bar is drawn. Keep it cheap: it runs at your
-- refresh rate, and a slow handler here is felt as a slow terminal.
--
-- `set_status` replaces *this plugin's* segments; it never affects anyone else's.
function M.on_status_bar_render(ctx)
  ctx.set_status({
    { text = os.date("%H:%M"), foreground = "#8be9fd" },
  })
end

-- Called before the terminal or the keymap sees a key. Return true to swallow it.
-- Return nothing to let it through — which is what you want almost always.
function M.on_key(ctx, key)
  if key.ctrl and key.shift and key.chord == "ctrl+shift+j" then
    ctx.notify("caught ctrl+shift+j", "info")
    return true
  end
end

-- A tab became visible.
function M.on_tab_switch(ctx, e)
  ctx.log("switched to tab " .. e.index)
end

return M
