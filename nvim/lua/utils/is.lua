local M = {}

---Check if a value is a table
---@param value any
---@return boolean
function M.table(value)
  return type(value) == "table"
end

---Check if a path is executable
---@param path string
---@return boolean
function M.executable(path)
  return vim.fn.executable(path) == 1
end

---Check if a file is readable
---@param path string
---@return boolean
function M.file_readable(path)
  return vim.fn.filereadable(path) == 1
end

---Check if a path is a directory
---@param path string
---@return boolean
function M.directory(path)
  return vim.fn.isdirectory(path) == 1
end

---Check if a path is not a directory
---@param path string
---@return boolean
function M.not_directory(path)
  return not M.directory(path)
end

---Check if a value is an empty table
---@param value any
---@return boolean
function M.empty_table(value)
  return M.table(value) and vim.tbl_isempty(value) or (#value == 1 and value[1] == "")
end

---Check if a value is empty (nil, empty string, or empty table)
---@param value any
---@return boolean
function M.empty(value)
  return value == nil or value == "" or M.empty_table(value)
end

---Check if a value is not empty
---@param value any
---@return boolean
function M.not_empty(value)
  return not M.empty(value)
end

return M
