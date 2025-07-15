M = {}

---Iterate through all Lua modules in a config directory
---@param dir_path string Relative path from lua/ directory (e.g. "plugins/lsp/servers")
---@param callback fun(module: table, name: string) Callback for each loaded module
M.each_config = function(dir_path, callback)
  local full_path = vim.fn.stdpath("config") .. "/lua/" .. dir_path
  local files = vim.fn.glob(full_path .. "/*.lua", false, true)

  for _, file in ipairs(files) do
    local name = vim.fn.fnamemodify(file, ":t:r")

    if name ~= "init" then
      local ok, module = pcall(require, dir_path:gsub("/", ".") .. "." .. name)
      if ok and module then
        callback(module, name)
      end
    end
  end
end

return M
