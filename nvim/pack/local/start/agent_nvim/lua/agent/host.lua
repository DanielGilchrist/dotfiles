local cmd_utils = require("utils.cmd")
local is = require("utils.is")

---Whatever runs the sessions, asked through the `agent plugin`, `agent prompt`
---and `agent dump` commands, which route to the host that owns each session.
---@class AgentHost
local M = {}

---@param args string[]
---@return vim.SystemCompleted
local function agent(args)
  return vim.system(vim.list_extend({ "agent" }, args), { text = true }):wait()
end

---@param with_cwd boolean
---@return { name: string, cwd: string|nil }[]
local function live(with_cwd)
  local res = agent(with_cwd and { "plugin", "live", "--cwd" } or { "plugin", "live" })
  if not cmd_utils.success(res.code) then return {} end
  local out = {}
  for line in (res.stdout or ""):gmatch("[^\n]+") do
    local name, cwd = line:match("^([^\t]+)\t?(.*)$")
    -- `agents` is the zellij host's multi-pane viewport, not something to
    -- attach to from inside nvim.
    if name and name ~= "agents" then table.insert(out, { name = name, cwd = cwd ~= "" and cwd or nil }) end
  end
  return out
end

---@return string[]
function M.list_sessions()
  return vim.tbl_map(function(session) return session.name end, live(false))
end

---@param name string
---@return boolean
function M.session_exists(name)
  return vim.tbl_contains(M.list_sessions(), name)
end

---Cwd the host reports for a live session, nil if it reports none or the
---directory is gone.
---@param name string
---@return string|nil
function M.session_cwd(name)
  for _, session in ipairs(live(true)) do
    if session.name == name and session.cwd and is.directory(session.cwd) then return session.cwd end
  end
  return nil
end

---Type `text` into the session as a message and submit it. `agent prompt`
---refuses while the agent asks or boots, and says why.
---@param name string
---@param text string
---@return boolean ok
---@return string|nil why
function M.send(name, text)
  local res = agent({ "prompt", name, "--", text })
  if cmd_utils.success(res.code) then return true end
  return false, vim.trim(res.stderr or "")
end

---Answer a menu in the session with its numbered choice.
---@param name string
---@param number integer
---@return boolean ok
---@return string|nil why
function M.choose(name, number)
  local res = agent({ "plugin", "choice", name, tostring(number) })
  if cmd_utils.success(res.code) then return true end
  return false, vim.trim(res.stderr or "")
end

---The command that attaches a terminal to `name` on whichever host owns it.
---@param name string
---@return string[]|nil argv
---@return string|nil why
function M.attach_cmd(name)
  local res = agent({ "plugin", "argv", name })
  if not cmd_utils.success(res.code) then return nil, vim.trim(res.stderr or "") end
  local argv = {}
  for line in (res.stdout or ""):gmatch("[^\n]+") do table.insert(argv, line) end
  if #argv == 0 then return nil, "agent plugin argv printed nothing" end
  return argv
end

---What `name`'s terminal shows and has scrolled off, as plain lines, read
---off the UI thread. The host that owns the session answers, whichever it is.
---@param name string
---@param on_lines fun(lines: string[]|nil, why: string|nil)
function M.scrollback(name, on_lines)
  vim.system({ "agent", "dump", name, "--scrollback", "--plain" }, { text = true }, function(res)
    vim.schedule(function()
      if not cmd_utils.success(res.code) then return on_lines(nil, vim.trim(res.stderr or "")) end
      local text = (res.stdout or ""):gsub("%s+$", "")
      on_lines(vim.split(text, "\n", { plain = true }))
    end)
  end)
end

return M
