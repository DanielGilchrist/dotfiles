---@class AgentZellij
---@field list_sessions fun(): string[]
---@field session_exists fun(name: string): boolean
---@field write_chars fun(name: string, text: string): boolean
---@field submit fun(name: string): boolean
---@field kill fun(name: string): boolean
local M = {}

---@return string[]
function M.list_sessions()
  local res = vim.system({ "zellij", "list-sessions", "-s" }, { text = true }):wait()
  if res.code ~= 0 then return {} end
  ---@type string[]
  local out = {}
  for line in (res.stdout or ""):gmatch("[^\n]+") do
    local trimmed = vim.trim(line)
    -- Hide the `agents` meta-session — it's a multi-pane viewport, not
    -- something you want to attach to from inside nvim.
    if trimmed ~= "" and trimmed ~= "agents" then table.insert(out, trimmed) end
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

---@param name string
---@param text string
---@return boolean
function M.write_chars(name, text)
  local res = vim.system({ "zellij", "--session", name, "action", "write-chars", text }, { text = true }):wait()
  return res.code == 0
end

---@param name string
---@return boolean
function M.submit(name)
  local res = vim.system({ "zellij", "--session", name, "action", "write", "13" }, { text = true }):wait()
  return res.code == 0
end

---@param name string
---@return boolean
function M.kill(name)
  local res = vim.system({ "zellij", "delete-session", "--force", name }, { text = true }):wait()
  return res.code == 0
end

return M
