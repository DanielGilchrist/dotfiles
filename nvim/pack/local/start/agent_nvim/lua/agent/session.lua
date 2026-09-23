local config = require("agent.config")
local is = require("utils.is")

---@class AgentSession
---@field resolve fun(): string|nil Auto-resolve session name for current cwd
---@field cwd_branch fun(): string|nil
---@field repo_root fun(): string|nil
---@field session_cwd fun(name: string): string|nil
local M = {}

---Toplevel of whichever worktree cwd is inside — the correct root for
---attaching a repo/worktree-local session (`<leader>as`), reviewing the
---worktree's changes, etc.
---@return string|nil
function M.repo_root()
  local res = vim.system({ "git", "rev-parse", "--show-toplevel" }, { text = true }):wait()
  if res.code ~= 0 then return nil end
  return vim.trim(res.stdout)
end

---MAIN worktree of the current repo — never a nested worktree. Use this
---when provisioning a new worktree (`<leader>an`) so `agent` namespaces
---under ~/worktrees/<repo>/<branch> instead of nesting under whichever
---worktree we happened to spawn from.
---@return string|nil
function M.main_repo_root()
  local res = vim.system({ "git", "worktree", "list", "--porcelain" }, { text = true }):wait()
  if res.code ~= 0 then return nil end
  return (res.stdout or ""):match("^worktree ([^\n]+)")
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

---@param name string
---@return string|nil
local function worktree_cwd(name)
  -- Scans every immediate subdir (including dot-prefixed repo dirs like
  -- `~/worktrees/.config/…`, which `vim.fn.glob`'s `*` would miss).
  local handle = vim.uv.fs_scandir(config.worktrees_dir)
  if not handle then return nil end
  while true do
    local entry, t = vim.uv.fs_scandir_next(handle)
    if not entry then break end
    if t == "directory" then
      local candidate = config.worktrees_dir .. "/" .. entry .. "/" .. name
      if is.directory(candidate) then return candidate end
    end
  end
  return nil
end

---Best-effort lookup of the on-disk cwd for a given zellij session name.
---For a worktree agent the session name is the branch and the cwd is
---`~/worktrees/<repo>/<branch>`. Repo sessions (`<leader>as`, named after
---the repo basename) have no worktree, so fall back to the cwd zellij
---reports for the session's own pane. Nil if neither resolves.
---@param name string
---@return string|nil
function M.session_cwd(name)
  return worktree_cwd(name) or require("agent.zellij").session_cwd(name)
end

return M
