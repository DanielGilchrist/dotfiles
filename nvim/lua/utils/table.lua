local M = {}

---Removes a value from a table by key and returns the value.
---@generic T
---@param table table<string, T>
---@param key string
---@return T?
M.remove = function(table, key)
  local value = table[key]
  table[key] = nil

  return value
end

return M
