local lua_path = vim.fn.stdpath("config") .. "/lua"

return {
  lua_path = lua_path,
  ---@param relative_path string
  ---@return string
  absolute_path = function(relative_path)
    return lua_path .. relative_path
  end,
  ---Find the project root by walking up from the current buffer
  ---@param markers? string[] File/dir names that identify a project root (default: { ".git" })
  ---@return string # Project root path, falls back to cwd
  root = function(markers)
    return vim.fs.root(0, markers or { ".git" }) or vim.uv.cwd() or error("FAAAAAAAAAAH")
  end,
}
