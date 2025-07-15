M = {}

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

return M
