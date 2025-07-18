local lua_path = vim.fn.stdpath("config") .. "/lua"

return {
  lua_path = lua_path,
  ---@param relative_path string
  ---@return string
  absolute_path = function(relative_path)
    return lua_path .. relative_path
  end
}
