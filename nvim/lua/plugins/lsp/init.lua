local function load_servers()
  local servers = {}
  local setup = {}

  local servers_dir = vim.fn.stdpath("config") .. "/lua/plugins/lsp/servers"
  local files = vim.fn.glob(servers_dir .. "/*.lua", false, true)

  for _, file in ipairs(files) do
    local name = vim.fn.fnamemodify(file, ":t:r")
    local ok, config = pcall(require, "plugins.lsp.servers." .. name)

    if ok and config then
      if config.server then
        servers[name] = config.server
      end
      if config.setup then
        setup[name] = config.setup
      end
    end
  end

  return servers, setup
end

return {
  "neovim/nvim-lspconfig",
  opts = function(_, opts)
    local servers, setup = load_servers()

    opts.diagnostics.virtual_text = false

    opts.codelens = {
      enabled = true
    }

    opts.inlay_hints = {
      enabled = false,
    }

    opts.servers = servers
    opts.setup = setup
  end,
}
