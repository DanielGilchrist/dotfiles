local config = require("agent.config")

---@class AgentSession
---@field resolve fun(): string|nil Auto-resolve session name for current cwd
---@field cwd_branch fun(): string|nil
---@field repo_root fun(): string|nil
---@field session_cwd fun(name: string): string|nil
local M = {}

---@return string|nil
function M.repo_root()
  local res = vim.system({ "git", "rev-parse", "--show-toplevel" }, { text = true }):wait()
  if res.code ~= 0 then return nil end
  return vim.trim(res.stdout)
end

---@return string|nil
function M.cwd_branch()
  local cwd = vim.fn.getcwd()
  local prefix = config.worktrees_dir .. "/"
  if vim.startswith(cwd, prefix) then
    local rest = cwd:sub(#prefix + 1)
    local _, branch = rest:match("([^/]+)/([^/]+)")
    if branch then return branch end
  end
  return nil
end

---Resolve the session name for the current context. Only fires when cwd is
---inside a worktree — outside, we let the caller fall back to the active /
---last-attached agent instead (sending to "main" from the main repo is
---never what the user meant).
---@return string|nil
function M.resolve()
  return M.cwd_branch()
end

---Best-effort lookup of the on-disk cwd for a given zellij session name.
---For a worktree agent the session name is the branch and the cwd is
---`~/worktrees/<repo>/<branch>`. Returns nil if no matching worktree exists.
---Scans every immediate subdir (including dot-prefixed repo dirs like
---`~/worktrees/.config/…`, which `vim.fn.glob`'s `*` would miss).
---@param name string
---@return string|nil
function M.session_cwd(name)
  local handle = vim.uv.fs_scandir(config.worktrees_dir)
  if not handle then return nil end
  while true do
    local entry, t = vim.uv.fs_scandir_next(handle)
    if not entry then break end
    if t == "directory" then
      local candidate = config.worktrees_dir .. "/" .. entry .. "/" .. name
      if vim.fn.isdirectory(candidate) == 1 then return candidate end
    end
  end
  return nil
end

return M
