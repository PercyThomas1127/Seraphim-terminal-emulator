-- A panel listing the git status of the focused terminal's directory.
-- Open it from Extensions -> Git Status, or with Ctrl+Shift+G.

local state = { branch = nil, files = {}, error = nil, cwd = nil }

local function refresh()
  local cwd = terminal.cwd()
  if not cwd then
    state.error = "No directory yet (needs shell integration)."
    ui.refresh("git")
    return
  end
  state.cwd = cwd
  process.run({ "git", "-C", cwd, "status", "--porcelain=v1", "--branch" }, function(r)
    if not r.ok then
      state.error = (r.stderr ~= "" and r.stderr) or r.error or "not a git repository"
      state.files, state.branch = {}, nil
    else
      state.error, state.files = nil, {}
      for line in r.stdout:gmatch("[^\n]+") do
        if line:sub(1, 2) == "##" then
          state.branch = line:sub(4)
        else
          table.insert(state.files, { code = line:sub(1, 2), path = line:sub(4) })
        end
      end
    end
    ui.refresh("git")
  end)
end

ui.panel("git", {
  title = "Git Status",
  render = function()
    local w = { { type = "heading", text = state.cwd or "git" } }
    if state.error then
      table.insert(w, { type = "text", text = state.error, style = "dim" })
      return w
    end
    table.insert(w, { type = "kv", items = { { "Branch", state.branch or "?" }, { "Changed", tostring(#state.files) } } })
    local items = {}
    for _, f in ipairs(state.files) do
      local style = f.code:find("?") and "dim" or (f.code:find("M") and "warn" or "accent")
      table.insert(items, { text = f.path, detail = f.code, style = style })
    end
    if #items > 0 then
      table.insert(w, { type = "list", items = items })
    else
      table.insert(w, { type = "text", text = "Clean.", style = "good" })
    end
    return w
  end,
  on_key = function(key)
    if key == "r" then refresh() end
  end,
})

-- Refresh when a command finishes or the directory changes.
seraphim.on("command.completed", refresh, { throttle = 500 })
seraphim.on("session.cwd.changed", refresh, { throttle = 500 })
seraphim.on("interface.pane.focused", refresh, { throttle = 200 })

ui.command("git", { title = "Git Status", run = function() ui.open_panel("git", { place = "right" }); refresh() end })
ui.key("ctrl+shift+g", "git")
