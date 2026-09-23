local wezterm = require("wezterm")

---@class PathUtils
---@field basename fun(path: string): string
---@field file_exists fun(path: string): boolean
---@field starts_with fun(path: string, prefix: string): boolean
---@field shorten_home fun(path: string): string
local M = {}

---Last path segment, ignoring a trailing slash.
---@param path string
---@return string
function M.basename(path)
  return path:match("([^/]+)/?$") or ""
end

---True if a regular file exists at <path> and is readable.
---@param path string
---@return boolean
function M.file_exists(path)
  local f = io.open(path, "r")
  if not f then return false end
  f:close()
  return true
end

---@param path string
---@param prefix string
---@return boolean
function M.starts_with(path, prefix)
  return path:sub(1, #prefix) == prefix
end

---Replace a leading home dir with `~`.
---@param path string
---@return string
function M.shorten_home(path)
  return (path:gsub("^" .. wezterm.home_dir, "~"))
end

return M
