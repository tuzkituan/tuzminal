-- Inline suggestions from shell history, in the manner of fish and
-- zsh-autosuggestions: type `git ch`, see the rest of the last matching command in
-- dim text after the cursor, press right to take it.
--
-- Everything difficult here comes from one fact: Tuzminal has no shell integration.
-- No OSC 133 marking where a prompt ends, no OSC 7 naming the working directory.
-- `on_input_line` hands over the cursor row up to the cursor — prompt included — and
-- that prompt could be a nerd-font glyph, a git branch, or three lines of ASCII art.
--
-- Three things are worth reading before the code:
--
--   * The prompt is not parsed, it is *out-lived*. When a row is sitting at a bare
--     prompt we remember it as `base`, and from then on the typed text is
--     `line:sub(#base + 1)` — byte-exact, with no theory about what a prompt looks
--     like. The marker list below answers only "is this row at a bare prompt?", never
--     "where inside this row does the prompt end?". The second question is the one
--     every marker heuristic gets wrong, because `$ `, `# `, `% ` and `> ` all occur
--     inside real commands: `echo a > b`, `awk '{print $ 1}'`.
--
--   * For rows we did not watch from their prompt, we fall back to matching the
--     longest suffix of the row that prefixes a history entry. That needs no prompt
--     knowledge at all, and its mistakes are cosmetic — a fallback match is never
--     recorded, so it cannot poison the corpus.
--
--   * A password that reaches the corpus reappears as ghost text later, in front of
--     whoever is looking at the screen. So `is_recordable` is deliberately over-eager,
--     and it is applied to the history *file* as much as to this session: the user's
--     shell may already have written a password to `~/.zsh_history`, and without the
--     filter this plugin would put it back on screen on day one. A command not learned
--     costs one suggestion; a secret learned costs a secret.
--
-- Costs, because this sits on the keystroke path: one read of the last 128 KB of the
-- history file at startup, and per keystroke one hash lookup plus a bounded plain-text
-- scan. Nothing at all per frame — this plugin deliberately does not define
-- `on_status_bar_render`, which runs at the refresh rate.

local M = {}

-- ── Tunables ────────────────────────────────────────────────────────────────
--
-- Constants, because a plugin cannot read its own `[config]` table — it is parsed from
-- the manifest, stored, and passed to neither runtime (docs/PLUGINS.md, "What plugins
-- cannot do"). Putting them in `[config]` would be a lie the manifest tells the user.
-- In the order I would want them configurable if that changes: the accept chords,
-- MIN_TYPED_SUFFIX, MAX_HINT_BYTES, and a switch for session recording.
local MIN_TYPED_EXACT = 1 -- fish suggests from one character, and so do we — but only
-- when `base` makes the split exact rather than guessed.
local MIN_TYPED_SUFFIX = 3 -- the fallback is guessing, so make it earn the guess.
local MAX_ENTRIES = 3000 -- bounds startup parse time and bucket length; a command
-- 3000 commands ago is noise.
local MAX_ENTRY_BYTES = 512 -- longer than this is a pasted one-liner, not a command.
local MAX_HINT_BYTES = 160 -- the payload carries no pane width, so the plugin cannot
-- clip to the row; the host truncates, this just bounds
-- what crosses the queue.
local TAIL_BYTES = 128 * 1024
local WINDOW_BYTES = 320 -- how much of the row the fallback searches.
local BUCKET_SCAN = 400 -- a bounded miss beats a slow keystroke.
local STALE_SECONDS = 10 -- an accept older than this is not trusted.
local MIN_RECORD_BYTES = 3

-- Prompt endings, used *only* to recognise a bare prompt. Every one of these also
-- occurs inside real commands, which is precisely why they are not used to locate a
-- split point. Written without the trailing space — `bare_prompt` requires that
-- separately — and compared with plain `sub`, never as a Lua pattern, so none of them
-- needs escaping.
local MARKERS = { "$", "#", "%", ">", "❯", "➜", "✗", "»", "λ" }

-- Substrings meaning "there is probably a secret on this line", matched against a
-- lowercased copy so `-P` and `--PASSWORD` are covered. Over-eager on purpose: `-p%S`
-- also rejects `mkdir -pv dir`, and losing that suggestion is free.
local SECRET_HINTS = {
  "pass",
  "secret",
  "token",
  "credential",
  "bearer",
  "apikey",
  "api_key",
  "api%-key",
  "%-%-key",
  "privkey",
  "pgpassword",
  "sshpass",
  "%-p%S", -- mysql -pSECRET, docker login -pTOKEN
  "%-u%s?%S+:%S", -- curl -u user:password
  "://%S+:%S+@", -- https://user:password@host
}

-- ── Corpus ──────────────────────────────────────────────────────────────────
--
-- Indexed by value rather than by position, so recording a command during the session
-- is a `table.insert` into one short list instead of renumbering the whole corpus.
-- Both index tables are newest-first, which makes "most recent match wins" the natural
-- result of scanning forwards rather than something that needs sorting.
local by1, by3 = {}, {}
local seen, entries = {}, 0
local enabled, have_hint_api = true, true

local function starts_with(s, p)
  return #s >= #p and s:find(p, 1, true) == 1
end

-- Cut `s` to at most `limit` bytes without splitting a UTF-8 character. Ghost text made
-- of half a character is a mojibake bug reported against the renderer.
local function utf8_trim(s, limit)
  if #s <= limit then
    return s
  end
  local cut = limit
  while cut > 0 do
    local nxt = s:byte(cut + 1)
    if not nxt or nxt < 0x80 or nxt >= 0xC0 then
      break
    end
    cut = cut - 1
  end
  return s:sub(1, cut)
end

-- Everything sent to the shell goes through here.
--
-- This is the most important function in the file. `SendText` is written to the PTY
-- raw and unbracketed, exactly as if typed — so a corpus entry containing a newline
-- would not be suggested, it would be *executed*. Truncating at the first control byte
-- is the last line of defence and the one that matters.
local function safe_to_send(s)
  local stop = s:find("[\1-\31\127]")
  if stop then
    s = s:sub(1, stop - 1)
  end
  return utf8_trim(s, MAX_HINT_BYTES)
end

local function looks_like_a_blob(s)
  for token in s:gmatch("%S+") do
    -- 28+ characters drawn only from the base64/hex alphabet, carrying both letters
    -- and digits: a JWT, an API key or a hash far more often than an argument. `/` and
    -- `.` are excluded from the alphabet so long paths are not mistaken for secrets.
    if #token >= 28 and not token:find("[^%w%+=_%-]") and token:find("%d") and token:find("%a") then
      return true
    end
  end
  return false
end

local function is_recordable(cmd)
  if not cmd or #cmd < MIN_RECORD_BYTES or #cmd > MAX_ENTRY_BYTES then
    return false
  end
  if cmd:find("[\1-\31\127]") then
    return false
  end
  -- A leading space is the shell's own opt-out (`HISTCONTROL=ignorespace`,
  -- `hist_ignore_space`). Users already type it before secrets, so honouring it costs
  -- nothing and is the most respectful mitigation available. It also throws away
  -- fish's indented `when:` metadata, which is a happy accident.
  if cmd:byte(1) == 0x20 or cmd:byte(1) == 0x09 then
    return false
  end
  if not cmd:sub(1, 1):find("[%w_%./~%-\"'%$%(]") then
    return false
  end
  local low = cmd:lower()
  for _, pattern in ipairs(SECRET_HINTS) do
    if low:find(pattern) then
      return false
    end
  end
  return not looks_like_a_blob(cmd)
end

local function push(index, key, cmd, front)
  local bucket = index[key]
  if not bucket then
    bucket = {}
    index[key] = bucket
  end
  if front then
    table.insert(bucket, 1, cmd)
  else
    bucket[#bucket + 1] = cmd
  end
end

local function drop(index, key, cmd)
  local bucket = index[key]
  if not bucket then
    return
  end
  for i, e in ipairs(bucket) do
    if e == cmd then
      table.remove(bucket, i)
      return
    end
  end
end

local function remember(cmd, front)
  if seen[cmd] then
    if not front then
      return
    end
    -- A command typed again should become the freshest suggestion, so it is moved
    -- rather than skipped. The scan is over one bucket, once per Enter.
    drop(by3, cmd:sub(1, 3), cmd)
    drop(by1, cmd:sub(1, 1), cmd)
  else
    if entries >= MAX_ENTRIES and not front then
      return
    end
    seen[cmd] = true
    entries = entries + 1
  end
  push(by3, cmd:sub(1, 3), cmd, front)
  push(by1, cmd:sub(1, 1), cmd, front)
end

-- ── Matching ────────────────────────────────────────────────────────────────

local function match_prefix(typed)
  local n = #typed
  if n == 0 then
    return nil
  end
  if n == 1 then
    local b = by1[typed]
    return b and b[1] or nil -- newest-first, so this is O(1)
  end
  if n == 2 then
    local b = by1[typed:sub(1, 1)]
    if not b then
      return nil
    end
    local want = typed:byte(2)
    for i = 1, math.min(#b, BUCKET_SCAN) do
      local e = b[i]
      if #e > n and e:byte(2) == want then
        return e
      end
    end
    return nil
  end
  local b = by3[typed:sub(1, 3)]
  if not b then
    return nil
  end
  for i = 1, math.min(#b, BUCKET_SCAN) do
    local e = b[i]
    -- `find(.., true) == 1` rather than `sub(1, n) == typed`: no substring is
    -- allocated, and this runs once per candidate per keystroke.
    if #e > n and e:find(typed, 1, true) == 1 then
      return e
    end
  end
  return nil
end

-- The fallback for rows we did not watch from their prompt: the longest suffix of the
-- row that prefixes a history entry.
--
-- Candidates start at a token boundary only. Testing every byte offset is what makes a
-- naive version of this suggest `tuzminal --help` from the `tuzminal` inside
-- `~/hobby/tuzminal`. Searching for the 0x20 byte is UTF-8 safe: a space byte cannot
-- occur inside a multi-byte sequence, so no character is ever split.
local function match_suffix(line)
  if #line > WINDOW_BYTES then
    line = line:sub(-WINDOW_BYTES)
  end
  local starts, i = { 1 }, 1
  while true do
    local sp = line:find(" ", i, true)
    if not sp then
      break
    end
    starts[#starts + 1] = sp + 1
    i = sp + 1
  end
  -- Ascending start position == longest suffix first.
  for _, s in ipairs(starts) do
    local typed = line:sub(s)
    if #typed >= MIN_TYPED_SUFFIX and typed:sub(1, 1):find("[%w_%./~%-\"'%$%(]") then
      local hit = match_prefix(typed)
      if hit then
        return typed, hit
      end
    end
  end
  return nil
end

-- Is this row sitting at a bare prompt? If so it is a trustworthy baseline: everything
-- appended from here on is something the user typed.
local function bare_prompt(line)
  -- A prompt is a marker plus the whitespace that separates it from what you type.
  -- Requiring that whitespace is what tells `$ ` (a prompt) from `echo $` (not one),
  -- and it is why the marker list carries no trailing space of its own.
  if not line:find("[ \t]$") then
    return false
  end
  local trimmed = line:gsub("[ \t]+$", "")
  for _, m in ipairs(MARKERS) do
    if #trimmed >= #m and trimmed:sub(-#m) == m then
      return true
    end
  end
  return false
end

-- ── Per-pane state ──────────────────────────────────────────────────────────
--
-- `base`   the row as it looked at the prompt, when we know it
-- `typed`  what the user has added since; nil when we do not know
-- `exact`  `typed` came from `base` rather than from the fallback matcher, so it is
--          safe to record on Enter
-- `blocked` the shell is in a mode we cannot follow (ctrl+r, vi command mode)
local panes, active = {}, nil
local pending, last_hint = nil, ""

local function state(pane)
  local st = panes[pane]
  if not st then
    st = {}
    panes[pane] = st
  end
  return st
end

local function set_hint(ctx, text)
  -- Only when it changes: this is reached on every keystroke, and each call allocates
  -- a Command and crosses the queue.
  if text == last_hint then
    return
  end
  last_hint = text
  ctx.set_inline_hint(text)
end

-- Withdraw the suggestion. Deliberately does *not* touch `st.typed` or `st.exact`:
-- those describe the line, not the suggestion, and Enter still needs them to learn what
-- was typed. Conflating the two meant nothing was ever recorded, because every line
-- that had no match cleared the text it was about to be remembered by.
local function clear(ctx)
  pending = nil
  if ctx then
    set_hint(ctx, "")
  end
end

local function reset(st)
  if st then
    st.base, st.typed, st.exact, st.blocked, st.line = nil, nil, false, false, nil
  end
end

function M.on_input_line(ctx, e)
  if not enabled or not have_hint_api then
    return
  end
  local pane = e.pane
  local st = state(pane)
  active = pane
  local line = e.line or ""

  -- Identical row: a redraw, not an edit. The hint already published is still correct,
  -- and recomputing it would be work per frame rather than per keystroke.
  if line == st.line then
    return
  end
  st.line = line

  -- Nothing to the left of the cursor: a fresh row, no prompt seen yet.
  if line == "" then
    clear(ctx)
    st.base = nil
    return
  end

  if st.base and starts_with(line, st.base) then
    -- The good case: the typed text is exact, with no guessing involved.
    st.typed, st.exact = line:sub(#st.base + 1), true
  elseif bare_prompt(line) then
    -- A bare prompt is a baseline we can trust from here on.
    st.base, st.typed, st.exact = line, "", true
  else
    -- The row no longer extends our baseline and is not a bare prompt: a rewritten
    -- line, interleaved output, tab completion, or a program's own display. Fall back
    -- to guessing, and mark it as a guess so Enter refuses to learn from it.
    st.base, st.typed, st.exact = nil, nil, false
  end

  -- The one thing a plugin cannot work out for itself, and the reason the host reports
  -- it: appending a suggestion to the middle of a command would corrupt it.
  if st.blocked or not e.at_line_end then
    clear(ctx)
    return
  end

  local matched, entry
  if st.typed and #st.typed >= MIN_TYPED_EXACT then
    matched, entry = st.typed, match_prefix(st.typed)
  end
  if not entry then
    matched, entry = match_suffix(line)
  end
  if not entry then
    clear(ctx)
    return
  end

  local hint = safe_to_send(entry:sub(#matched + 1))
  if hint == "" then
    clear(ctx)
    return
  end
  pending = { pane = pane, hint = hint, at = os.time() }
  set_hint(ctx, hint)
end

-- ── Keys ────────────────────────────────────────────────────────────────────
--
-- Matched here as literal chords rather than through `register_keybind`, because a
-- bound chord is dispatched and consumed unconditionally — there is no way for a
-- registered binding to decline a key. `right` and `end` have to reach the shell
-- whenever there is nothing to accept, and `on_key` is the only place that can say
-- "not this time".
local ACCEPT_ALL = { right = true, ["end"] = true, ["ctrl+e"] = true, ["ctrl+space"] = true }
local ACCEPT_WORD = { ["alt+right"] = true }
local SUBMIT = { enter = true, ["shift+enter"] = true, ["ctrl+m"] = true, ["ctrl+j"] = true }

-- Shell modes we cannot follow from the outside. `ctrl+r` replaces the row with its own
-- search UI; `escape` may enter vi command mode, where `right` moves the cursor and
-- accepting would type into a command rather than a line. Both stay blocked until the
-- line is submitted or abandoned, so vi users get suggestions while inserting and
-- silence while editing — the safe half of the trade.
local BLOCKS = { ["ctrl+r"] = true, ["ctrl+s"] = true, escape = true, ["ctrl+x"] = true }
local RELEASES = { ["ctrl+c"] = true, ["ctrl+g"] = true }

local function commit(ctx, st)
  -- Only text watched from a bare prompt is ever learned. A guessed split point would
  -- file prompt fragments as commands, and a corpus with prompt fragments in it makes
  -- the fallback matcher match more prompt fragments — the failure compounds.
  if not (st and st.exact and st.typed) then
    return
  end
  local cmd = st.typed
  if cmd == "" then
    return
  end
  if not is_recordable(cmd) then
    ctx.log("suggest: not recording a line that looks like a secret")
    return
  end
  remember(cmd, true)
end

local function accept(ctx, whole)
  local hint = pending.hint
  local chunk = hint
  if not whole then
    -- One word: any leading spaces plus the next run of non-space bytes. Split on
    -- whitespace rather than on zsh's WORDCHARS because `alt+right` over a path should
    -- step across `feature/branch-name`, not through it. A sub-word step is the first
    -- thing I would make configurable.
    chunk = hint:match("^[ \t]*[^ \t]+") or hint
  end
  chunk = safe_to_send(chunk)
  if chunk == "" then
    return false
  end
  ctx.send_text(chunk)
  -- No local prediction of the result. The shell echoes what it actually did and the
  -- next `on_input_line` recomputes from that; guessing the echo is how these plugins
  -- drift out of step with the line they are describing.
  set_hint(ctx, "")
  pending = nil
  return true
end

function M.on_key(ctx, key)
  local c = key.chord
  local st = active and panes[active]

  if SUBMIT[c] then
    -- There is no "command submitted" event, so Enter is where this session's own
    -- history comes from. Never swallowed: the shell must still see it.
    commit(ctx, st)
    reset(st)
    clear(ctx)
    return
  end

  if enabled and (ACCEPT_ALL[c] or ACCEPT_WORD[c]) then
    -- "Only accept at the end of the line" needs no check here, because a hint only
    -- exists if `on_input_line` saw `at_line_end`. That is also what lets `right`
    -- through untouched the rest of the time.
    local fresh = pending and pending.pane == active and (os.time() - pending.at) <= STALE_SECONDS
    if fresh and st and not st.blocked and accept(ctx, ACCEPT_ALL[c] == true) then
      return true
    end
    return -- nothing to accept: the key is the shell's
  end

  if not st then
    return
  end
  if RELEASES[c] then
    st.blocked = false
    reset(st)
    clear(ctx)
  elseif BLOCKS[c] then
    st.blocked = true
    clear(ctx)
  end
end

-- ── Invalidation ────────────────────────────────────────────────────────────

function M.on_pane_closed(_ctx, e)
  panes[e.pane] = nil
  if active == e.pane then
    active = nil
  end
  if pending and pending.pane == e.pane then
    pending, last_hint = nil, ""
  end
end

-- The ghost text describes a row that is no longer on screen, and accepting into it
-- would type into another tab's shell. The host stops drawing it; we stop believing it.
function M.on_tab_switch(ctx, _e)
  clear(ctx)
  active = nil
end

-- ── History file ────────────────────────────────────────────────────────────

local function read_tail(path)
  if not path or path == "" then
    return nil
  end
  local f = io.open(path, "rb")
  if not f then
    return nil
  end
  local size = f:seek("end") or 0
  local text
  if size > TAIL_BYTES then
    -- The tail, not the file. A 10 MB history is ~200k lines, and parsing that in Lua
    -- would pass the 250 ms callback budget, be aborted by the instruction hook, and
    -- leave the plugin with *no* corpus at all — the failure mode is total, not
    -- partial. History files are append-ordered, so the tail is also the only part a
    -- most-recent-first ranking could ever surface.
    f:seek("set", size - TAIL_BYTES)
    text = f:read("a") or ""
    text = text:match("\n(.*)$") or "" -- the first line is cut in half
  else
    f:seek("set", 0)
    text = f:read("a") or ""
  end
  f:close()
  return text
end

local function parse_into(text)
  local lines, skip = {}, false
  for raw in text:gmatch("[^\n]+") do
    if skip then
      -- A zsh entry continued across lines. Consumed and discarded: a multi-line
      -- command cannot be offered as ghost text, and left alone its second line would
      -- be learned as a command in its own right.
      skip = raw:sub(-1) == "\\"
    else
      local cmd = raw
      local zsh = cmd:match("^: %d+:%d+;(.*)$") -- zsh EXTENDED_HISTORY
      if zsh then
        cmd = zsh
      end
      local fish = cmd:match("^%- cmd: (.*)$") -- fish's YAML-ish history
      if fish then
        cmd = fish
      end
      if cmd:match("^#%d+$") then -- bash HISTTIMEFORMAT stamp
        cmd = nil
      end
      if cmd and cmd:sub(-1) == "\\" then
        skip, cmd = true, nil
      end
      if cmd then
        lines[#lines + 1] = cmd
      end
    end
  end
  -- Backwards, so the newest entry wins deduplication and lands first in every bucket.
  -- `remember(.., false)` appends, so files read later rank as older.
  for i = #lines, 1, -1 do
    local cmd = lines[i]
    if is_recordable(cmd) then
      remember(cmd, false)
      if entries >= MAX_ENTRIES then
        return
      end
    end
  end
end

local function history_paths()
  local home = os.getenv("HOME") or ""
  local shell = os.getenv("SHELL") or ""
  -- `$HISTFILE` first because it costs one `getenv`, but it is usually *not* exported:
  -- it is a shell variable, so what we would see here is the desktop session's
  -- environment rather than the shell in the pane. The literal paths are what actually
  -- resolves, ordered so the current shell's own history is read first.
  local ordered = { os.getenv("HISTFILE") }
  if shell:find("fish", 1, true) then
    ordered[#ordered + 1] = home .. "/.local/share/fish/fish_history"
  end
  if shell:find("bash", 1, true) then
    ordered[#ordered + 1] = home .. "/.bash_history"
  end
  ordered[#ordered + 1] = home .. "/.zsh_history"
  ordered[#ordered + 1] = home .. "/.bash_history"
  ordered[#ordered + 1] = home .. "/.local/share/fish/fish_history"

  local out, done = {}, {}
  for _, p in ipairs(ordered) do
    if p and p ~= "" and not done[p] then
      done[p] = true
      out[#out + 1] = p
    end
  end
  return out
end

local function load_history()
  by1, by3, seen, entries = {}, {}, {}, 0
  for _, path in ipairs(history_paths()) do
    if entries >= MAX_ENTRIES then
      break
    end
    local text = read_tail(path)
    if text then
      parse_into(text)
    end
  end
end

function M.on_startup(ctx)
  if type(ctx.set_inline_hint) ~= "function" then
    -- Better to go quiet than to raise on every keystroke until the host disables us
    -- after three failures, with a warning nobody can act on.
    have_hint_api = false
    ctx.log("suggest: this host has no inline hint support; the plugin is idle")
    return
  end
  ctx.register_command("toggle", "Turn inline history suggestions on or off")
  ctx.register_command("reload_history", "Re-read the shell history file")
  load_history()
  ctx.log("suggest: " .. entries .. " history entries")
end

function M.on_command(ctx, e)
  if e.name == "toggle" then
    enabled = not enabled
    if not enabled then
      clear(ctx)
    end
    ctx.notify(enabled and "suggestions on" or "suggestions off", "info")
  elseif e.name == "reload_history" then
    -- Manual rather than periodic: zsh only writes its history file when the shell
    -- exits unless INC_APPEND_HISTORY is set, so polling would mostly re-read the same
    -- bytes, and the only handler firing often enough to poll from is
    -- `on_status_bar_render` — where a 128 KB read is a visible hitch every frame.
    load_history()
    ctx.notify("history reloaded: " .. entries .. " entries", "info")
  end
end

return M
