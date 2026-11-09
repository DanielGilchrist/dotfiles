local wezterm = require("wezterm")
local notify = require("utils.notify")
local process = require("utils.process")
local shell = require("utils.shell")

local M = {}

---@type string|nil
local editor_name

---The basename of $EDITOR as the login shell sets it, which is what the
---editor's process is called in a pane.
---@return string|nil
local function editor()
  if editor_name then return editor_name end
  local ok, out = shell.run(shell.login("echo $EDITOR"))
  local name = ok and out:match("([^/%s]+)$") or nil
  editor_name = name
  return name
end

---@param name string
---@param cwd string
---@return MuxTab|nil, Pane|nil
local function find_pane(name, cwd)
  local function is_editor_in_cwd(info)
    return info.name == name and info.cwd == cwd
  end
  for _, mux_window in ipairs(wezterm.mux.all_windows()) do
    for _, tab in ipairs(mux_window:tabs()) do
      for _, p in ipairs(tab:panes()) do
        -- nil for panes that aren't local
        if process.any(p:get_foreground_process_info(), is_editor_in_cwd) then return tab, p end
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

---@param window Window
---@param pane Pane
---@param mode string
---@param cwd string
local function open(window, pane, mode, cwd)
  local args = shell.login("exec $EDITOR")
  if mode == "right" then
    pane:split({ direction = "Right", size = 0.5, cwd = cwd, args = args })
  else
    window:mux_window():spawn_tab({ cwd = cwd, args = args })
  end
end

-- Listen for `agent-edit=<mode>|<cwd>` from `agent` TUI actions, where
-- <mode> is `tab` or `right`. An editor a pane already runs in <cwd> is
-- focused, in its window too, instead of opening a second one.
function M.register()
  wezterm.on("user-var-changed", function(window, pane, var, value)
    if var ~= "agent-edit" then return end
    local mode, cwd = (value or ""):match("^(%a+)|(.+)$")
    if mode ~= "tab" and mode ~= "right" then
      notify(window, "agent", "agent-edit: malformed payload")
      return
    end
    -- Over SSH the TUI's paths are the remote machine's.
    if not wezterm.run_child_process({ "test", "-d", cwd }) then
      notify(window, "agent", "agent-edit: " .. cwd .. " isn't on this machine")
      return
    end

    local name = editor()
    local tab, existing
    if name then tab, existing = find_pane(name, cwd) end
    if tab and existing then
      focus(tab, existing)
    else
      open(window, pane, mode, cwd)
    end
  end)
end

return M
