local wezterm = require("wezterm")
local shell = require("utils.shell")
local worktree = require("utils.worktree")

---@class WorktreeChoice
---@field label string
---@field id string

---@class WorktreePickerModule
---@field open fun(window: any, pane: any): nil
local M = {}

---@return WorktreeChoice[]
local function list_worktrees()
  ---@type WorktreeChoice[]
  local choices = {}
  for _, entry in ipairs(worktree.list()) do
    table.insert(choices, { label = entry.repo .. "/" .. entry.branch, id = entry.path })
  end
  return choices
end

M.open = function(window, pane)
  window:perform_action(wezterm.action.InputSelector({
    title = "Select worktree",
    choices = list_worktrees(),
    fuzzy = true,
    action = wezterm.action_callback(function(inner_window, _, id)
      if not id then return end
      -- The selector overlay is the active pane when this fires and is
      -- already being torn down, so target the pane the picker opened from.
      -- cd inside fish rather than trusting --cwd (wez/wezterm#4618).
      local quoted = shell.quote(id)
      inner_window:perform_action(
        wezterm.action.SpawnCommandInNewTab({
          args = { shell.fish(), "-i", "-c", "cd " .. quoted .. "; or echo 'cannot cd into '" .. quoted .. "; exec fish -i" },
        }),
        pane
      )
    end),
  }), pane)
end

return M
