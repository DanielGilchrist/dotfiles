local wezterm = require("wezterm")

---Helpers for building and running shell commands from wezterm. Wezterm
---spawns with a minimal PATH (no /opt/homebrew/bin), so binaries are
---resolved through a login shell on first use and memoised.
---@class ShellUtils
---@field which fun(name: string, fallback: string): string
---@field fish fun(): string
---@field quote fun(s: string): string
---@field trim fun(s: string|nil): string
---@field run fun(args: string[]): boolean, string
local M = {}

---@type table<string, string>
local resolved = {}

---Absolute path of <name> via the login shell, or <fallback>. Can't be
---called at module-load time — `run_child_process` needs a coroutine.
---@param name string
---@param fallback string
---@return string
function M.which(name, fallback)
  if resolved[name] then return resolved[name] end

  local _, out = wezterm.run_child_process({ "/bin/sh", "-lc", "command -v " .. name })
  local path = M.trim(out)
  resolved[name] = path ~= "" and path or fallback

  return resolved[name]
end

---@return string
function M.fish()
  return M.which("fish", "/opt/homebrew/bin/fish")
end

---Single-quote a string for safe inclusion in a fish-shell command.
---@param s string
---@return string
function M.quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

---Strip trailing whitespace (command output almost always ends in a newline).
---@param s string|nil
---@return string
function M.trim(s)
  if not s then return "" end
  return (s:gsub("%s+$", ""))
end

---Run a child process and return success plus trimmed stdout.
---@param args string[]
---@return boolean success, string stdout
function M.run(args)
  local ok, out = wezterm.run_child_process(args)
  return ok == true, M.trim(out)
end

return M
