local loader = require("utils.plugin_loader")

local setup = {}
local servers = {}

loader.each_config("plugins/lsp/servers", function(config, name)
  if config.setup then
    setup[name] = config.setup
  end

  if config.server then
    servers[name] = config.server
  end
end)

return {
  "neovim/nvim-lspconfig",
  opts = {
    setup = setup,
    servers = servers,
    codelens = {
      enabled = true,
    },
    diagnostics = {
      virtual_text = false,
    },
    inlay_hints = {
      enabled = false,
    },
  },
}
