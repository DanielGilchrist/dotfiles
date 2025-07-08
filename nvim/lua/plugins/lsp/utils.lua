local M = {}

function M.disable_format(client)
  client.server_capabilities.documentFormattingProvider = false
  client.server_capabilities.documentRangeFormattingProvider = false
end

function M.asdf_shim(command)
  return { vim.fn.expand("~/.asdf/shims/" .. command) }
end

function M.gem_available(gem_name)
  local result = vim.fn.system("bundle show " .. gem_name .. " 2>/dev/null")
  return vim.v.shell_error == 0
end

return M
