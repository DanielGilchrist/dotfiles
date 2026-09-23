local cmd_utils = require("utils.cmd")
local is = require("utils.is")

---@class AgentZellij
---@field list_sessions fun(): string[]
---@field session_exists fun(name: string): boolean
---@field session_cwd fun(name: string): string|nil
---@field write_chars fun(name: string, text: string): boolean
---@field submit fun(name: string): boolean
---@field kill fun(name: string): boolean
local M = {}

---@return string[]
function M.list_sessions()
  local res = vim.system({ "zellij", "list-sessions", "-s" }, { text = true }):wait()
  if not cmd_utils.success(res.code) then return {} end
  ---@type string[]
  local out = {}
  for line in (res.stdout or ""):gmatch("[^\n]+") do
    local trimmed = vim.trim(line)
    -- Hide the `agents` meta-session — it's a multi-pane viewport, not
    -- something you want to attach to from inside nvim.
    if is.not_empty(trimmed) and trimmed ~= "agents" then table.insert(out, trimmed) end
  end
  return out
end

---@param name string
---@return boolean
function M.session_exists(name)
  for _, s in ipairs(M.list_sessions()) do
    if s == name then return true end
  end
  return false
end

---Cwd of the session's first terminal pane, as reported by zellij. Nil if
---the session isn't reachable or the directory no longer exists.
---@param name string
---@return string|nil
function M.session_cwd(name)
  local res = vim.system({ "zellij", "--session", name, "action", "list-panes", "--json" }, { text = true }):wait()
  if not cmd_utils.success(res.code) then return nil end
  local ok, panes = pcall(vim.json.decode, res.stdout or "")
  if not ok or not is.table(panes) then return nil end
  for _, pane in ipairs(panes) do
    local cwd = not pane.is_plugin and pane.pane_cwd
    if is.string(cwd) and is.directory(cwd) then return cwd end
  end
  return nil
end

---@param name string
---@param text string
---@return boolean
function M.write_chars(name, text)
  local res = vim.system({ "zellij", "--session", name, "action", "write-chars", text }, { text = true }):wait()
  return cmd_utils.success(res.code)
end

---@param name string
---@return boolean
function M.submit(name)
  local res = vim.system({ "zellij", "--session", name, "action", "write", "13" }, { text = true }):wait()
  return cmd_utils.success(res.code)
end

---@param name string
---@return boolean
function M.kill(name)
  local res = vim.system({ "zellij", "delete-session", "--force", name }, { text = true }):wait()
  return cmd_utils.success(res.code)
end

return M
