local wezterm = require("wezterm")
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
      -- The InputSelector overlay pane is torn down before this fires; use
      -- the window's current active pane as the perform_action target.
      local target = inner_window:active_pane()
      if not target then return end
      inner_window:perform_action(
        wezterm.action.SpawnCommandInNewTab({ cwd = id }),
        target
      )
    end),
  }), pane)
end

return M
