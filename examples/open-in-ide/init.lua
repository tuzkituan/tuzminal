-- Buttons that open the current directory in whichever editors are installed.
--
-- Two things are worth noticing, because they are what make this workable as a
-- plugin rather than a built-in:
--
--   * It never asks the terminal where the shell is. It sends `code .`, so the
--     shell's own working directory decides — which is more correct than the
--     built-in version was, since that read /proc and only worked on Linux.
--
--   * Detection is a single `io.open` per editor at startup. Doing it per frame
--     would be a stat per editor per redraw.

local M = {}

-- Two-letter monograms rather than names: the status bar is narrow, and eight
-- editors spelled out would be the whole bar. Distinct so two installed side by
-- side are never a coin flip.
local EDITORS = {
  { id = "code",     icon = "VS", command = "code" },
  { id = "cursor",   icon = "Cu", command = "cursor" },
  { id = "windsurf", icon = "Wi", command = "windsurf" },
  { id = "zed",      icon = "Ze", command = "zed" },
  { id = "subl",     icon = "Su", command = "subl" },
  { id = "idea",     icon = "IJ", command = "idea" },
  { id = "nvim",     icon = "Nv", command = "nvim" },
}

local SEARCH = { "/usr/bin/", "/usr/local/bin/", "/bin/", "/snap/bin/" }

local found = {}

local function installed(command)
  for _, dir in ipairs(SEARCH) do
    local f = io.open(dir .. command, "r")
    if f then
      f:close()
      return true
    end
  end
  return false
end

function M.on_startup(ctx)
  for _, editor in ipairs(EDITORS) do
    if installed(editor.command) then
      table.insert(found, editor)
    end
  end
  ctx.log("open-in-ide: found " .. #found .. " editor(s)")
end

-- Rebuilt every frame, so an editor installed while the terminal is running shows
-- up at the next restart rather than never. The table is tiny; the cost is the
-- allocation, not the work.
function M.on_status_bar_render(ctx)
  local segments = {}
  for _, editor in ipairs(found) do
    -- An `id` is what makes a segment clickable. Without one it is drawn and
    -- ignored, which is what a clock wants and this does not.
    table.insert(segments, { text = editor.icon, id = editor.id })
  end
  ctx.set_status(segments)
end

function M.on_status_segment_click(ctx, e)
  for _, editor in ipairs(found) do
    if editor.id == e.id then
      -- `.` rather than an absolute path: the shell expands it against its own
      -- directory, so this needs no way to ask where that is.
      ctx.send_text(editor.command .. " .")
      return
    end
  end
end

return M
