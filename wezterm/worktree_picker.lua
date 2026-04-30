local wezterm = require("wezterm")

---@class WorktreeChoice
---@field label string
---@field id string

---@class WorktreePickerModule
---@field open fun(window: any, pane: any): nil
local M = {}

local WORKTREES_DIR = wezterm.home_dir .. "/worktrees"

---@return WorktreeChoice[]
local function list_worktrees()
  ---@type WorktreeChoice[]
  local choices = {}
  local _, out = wezterm.run_child_process({
    "find", WORKTREES_DIR,
    "-mindepth", "2", "-maxdepth", "2", "-type", "d",
  })
  if not out then return choices end

  for path in out:gmatch("[^\n]+") do
    local has_git = wezterm.run_child_process({ "test", "-e", path .. "/.git" })
    if has_git then
      local repo, branch = path:match("/worktrees/([^/]+)/([^/]+)$")
      if repo and branch then
        table.insert(choices, { label = repo .. "/" .. branch, id = path })
      end
    end
  end
  return choices
end

M.open = function(window, pane)
  window:perform_action(wezterm.action.InputSelector({
    title = "Select worktree",
    choices = list_worktrees(),
    fuzzy = true,
    action = wezterm.action_callback(function(inner_window, inner_pane, id, _label)
      if not id then return end
      inner_window:perform_action(
        wezterm.action.SpawnCommandInNewTab({ cwd = id }),
        inner_pane
      )
    end),
  }), pane)
end

return M
