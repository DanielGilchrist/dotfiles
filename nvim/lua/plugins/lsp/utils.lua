local M = {}

---Disable formatting capabilities for an LSP client
---@param client vim.lsp.Client The LSP client to modify
function M.disable_format(client)
  client.server_capabilities.documentFormattingProvider = false
  client.server_capabilities.documentRangeFormattingProvider = false
end

---Get the path to a command via asdf shims
---@param command string The command name
---@return string[] # Array with the full path to the asdf shim
function M.asdf_shim(command)
  return { vim.fn.expand("~/.asdf/shims/" .. command) }
end

---Check if a gem is available in the current bundle
---@param gem_name string The name of the gem to check
---@return boolean # True if the gem is available
function M.gem_available(gem_name)
  vim.fn.system("bundle show " .. gem_name .. " 2>/dev/null")
  return vim.v.shell_error == 0
end

---Iterate over LSP clients with optional filter
---@param filter vim.lsp.get_clients.Filter|nil Filter for clients
---@param callback fun(client: vim.lsp.Client): boolean? Callback function, return true to break
function M.each_client(filter, callback)
  local clients = vim.lsp.get_clients(filter)
  for _, client in ipairs(clients) do
    if callback(client) then
      break
    end
  end
end

return M
