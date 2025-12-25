local M = {}

---Check if a string starts with a prefix
---@param s string The string to check
---@param prefix string The prefix to look for
---@return boolean
function M.starts_with(s, prefix)
  return s:sub(1, #prefix) == prefix
end

---Check if a string includes a pattern
---@param s string The string to search in
---@param pattern string The pattern to search for
---@param plain? boolean If true, pattern is treated as plain text (default: false)
---@return boolean
function M.includes(s, pattern, plain)
  plain = plain == nil and false or plain
  return s:find(pattern, 1, plain) ~= nil
end

---Check if a string excludes (does not include) a pattern
---@param s string The string to search in
---@param pattern string The pattern to search for
---@param plain? boolean If true, pattern is treated as plain text (default: false)
---@return boolean
function M.excludes(s, pattern, plain)
  return not M.includes(s, pattern, plain)
end

---Trim whitespace from both ends of a string
---@param s string The string to trim
---@return string
function M.trim(s)
  return s:match("^%s*(.-)%s*$")
end

return M
