local wezterm = require("wezterm")
local path_utils = require("utils.path")

---@class WorktreeEntry
---@field repo string
---@field branch string
---@field path string

---Worktrees live at ~/worktrees/<repo>/<branch>. The directory on disk is
---the source of truth (zellij sessions can die independently), so listing
---scans the filesystem rather than asking zellij.
---@class WorktreeUtils
---@field DIR string
---@field REPOS_DIR string
---@field git_dirs fun(root: string): string[]
---@field list fun(): WorktreeEntry[]
---@field contains fun(path: string): boolean
local M = {
  DIR = wezterm.home_dir .. "/worktrees",
  REPOS_DIR = wezterm.home_dir .. "/Documents/repos",
}

---Directories exactly two levels below <root> that contain a `.git`
---(a checkout or a worktree link file).
---@param root string
---@return string[]
function M.git_dirs(root)
  ---@type string[]
  local dirs = {}
  local _, out = wezterm.run_child_process({
    "find", root, "-mindepth", "2", "-maxdepth", "2", "-type", "d",
  })
  if not out then return dirs end

  for dir in out:gmatch("[^\n]+") do
    if wezterm.run_child_process({ "test", "-e", dir .. "/.git" }) then
      table.insert(dirs, dir)
    end
  end

  return dirs
end

---@return WorktreeEntry[]
function M.list()
  ---@type WorktreeEntry[]
  local entries = {}

  for _, dir in ipairs(M.git_dirs(M.DIR)) do
    local repo, branch = dir:match("/worktrees/([^/]+)/([^/]+)$")
    if repo and branch then
      table.insert(entries, { repo = repo, branch = branch, path = dir })
    end
  end

  return entries
end

---True if <path> is inside ~/worktrees.
---@param path string
---@return boolean
function M.contains(path)
  return path_utils.starts_with(path, M.DIR .. "/")
end

return M
