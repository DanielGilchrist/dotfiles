local wezterm = require("wezterm")
local notify = require("utils.notify")
local process = require("utils.process")
local shell = require("utils.shell")
local worktree = require("utils.worktree")

local M = {}

---Where install.sh puts the binary. Called directly rather than through a
---login shell because this runs on the GUI thread and the scan needs no PATH.
local AGENT = wezterm.home_dir .. "/.local/bin/agent"

---The client pids of the windows showing <name>, by the same scan the TUI
---uses, so the two agree on what counts as a window.
---@param name string
---@return table<integer, true>|nil
local function window_pids(name)
  local ok, out = wezterm.run_child_process({ AGENT, "windows", name })
  if not ok then return nil end
  local pids = {}
  for pid in (out or ""):gmatch("%d+") do
    pids[tonumber(pid)] = true
  end
  return pids
end

---@param pids table<integer, true>
---@return MuxTab|nil, Pane|nil
local function find_pane(pids)
  local function is_client(info)
    return pids[info.pid] == true
  end
  for _, mux_window in ipairs(wezterm.mux.all_windows()) do
    for _, tab in ipairs(mux_window:tabs()) do
      for _, p in ipairs(tab:panes()) do
        -- nil for panes that aren't local
        if process.any(p:get_foreground_process_info(), is_client) then return tab, p end
      end
    end
  end
  return nil, nil
end

---@param tab MuxTab
---@param pane Pane
local function focus(tab, pane)
  local gui = tab:window():gui_window()
  tab:activate()
  pane:activate()
  if gui then gui:focus() end
end

-- A repo session has no worktree, and `agent attach` would plan one for it.
---@param name string
---@param cwd string
local function command_for(name, cwd)
  if worktree.contains(cwd) then
    return "exec agent attach " .. shell.quote(name)
  end
  return "exec agent repo"
end

---@param window Window
---@param pane Pane
---@param mode string
---@param name string
---@param cwd string
local function open(window, pane, mode, name, cwd)
  local args = shell.login(command_for(name, cwd))
  if mode == "right" then
    pane:split({ direction = "Right", size = 0.5, cwd = cwd, args = args })
  else
    window:mux_window():spawn_tab({ cwd = cwd, args = args })
  end
end

-- Listen for `agent-open=<mode>|<name>|<cwd>` from `agent` TUI actions,
-- where <mode> is `tab` or `right`. An agent a pane already shows is
-- focused, in its window too, instead of getting a second client.
function M.register()
  wezterm.on("user-var-changed", function(window, pane, var, value)
    if var ~= "agent-open" then return end
    local mode, name, cwd = (value or ""):match("^(%a+)|([^|]+)|(.+)$")
    if (mode ~= "tab" and mode ~= "right") or not name:match("^[%w._-]+$") then
      notify(window, "agent", "agent-open: malformed payload")
      return
    end
    -- Over SSH the TUI's paths are the remote machine's.
    if not wezterm.run_child_process({ "test", "-d", cwd }) then
      notify(window, "agent", "agent-open: " .. cwd .. " isn't on this machine")
      return
    end

    local pids = window_pids(name)
    if not pids then
      notify(window, "agent", "agent-open: `agent windows " .. name .. "` failed")
      return
    end
    local tab, existing = find_pane(pids)
    if tab and existing then
      focus(tab, existing)
    else
      open(window, pane, mode, name, cwd)
    end
  end)
end

return M
