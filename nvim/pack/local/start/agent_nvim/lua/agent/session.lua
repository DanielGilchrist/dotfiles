local config = require("agent.config")

---@class AgentSession
---@field resolve fun(): string|nil Auto-resolve session name for current cwd
---@field cwd_branch fun(): string|nil
---@field repo_root fun(): string|nil
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
  local res = vim.system({ "git", "branch", "--show-current" }, { text = true }):wait()
  if res.code ~= 0 then return nil end
  local branch = vim.trim(res.stdout)
  if branch == "" then return nil end
  return branch
end

---@return string|nil
function M.resolve()
  local override = vim.b.agent_session
  if override and override ~= "" then return override end
  return M.cwd_branch()
end

return M
