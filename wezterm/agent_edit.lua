local wezterm = require("wezterm")
local notify = require("utils.notify")
local shell = require("utils.shell")

local M = {}

---@param window Window
---@param pane Pane
---@param mode string
---@param cwd string
local function open(window, pane, mode, cwd)
  -- wezterm ignores `cwd` here and starts in the emitting pane's directory.
  local args = shell.login("cd " .. shell.quote(cwd) .. " && exec $EDITOR")
  if mode == "right" then
    pane:split({ direction = "Right", size = 0.5, cwd = cwd, args = args })
  else
    window:mux_window():spawn_tab({ cwd = cwd, args = args })
  end
end

-- Listen for `agent-edit=<mode>|<cwd>` from `agent` TUI actions, where
-- <mode> is `tab` or `right`.
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

    open(window, pane, mode, cwd)
  end)
end

return M
