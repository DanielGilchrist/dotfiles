local M = {}

---Read the contents of a file
---@param path string
---@return string?
M.read = function(path)
  local file = io.open(path, "r")

  if not file then
    return nil
  end

  local contents = file:read("*a")
  file:close()

  return contents
end

---@param path string
---@param mode "w"|"a"
---@param contents string
---@return boolean
local function put(path, mode, contents)
  local file = io.open(path, mode)

  if not file then
    return false
  end

  file:write(contents)
  file:close()

  return true
end

---Replace the contents of a file, creating it if needed
---@param path string
---@param contents string
---@return boolean # false if the file could not be opened for writing
M.write = function(path, contents)
  return put(path, "w", contents)
end

---Append to a file, creating it if needed
---@param path string
---@param contents string
---@return boolean # false if the file could not be opened for writing
M.append = function(path, contents)
  return put(path, "a", contents)
end

return M
