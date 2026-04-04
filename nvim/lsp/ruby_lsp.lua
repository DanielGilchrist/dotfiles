local lsp_utils = require("lsp.utils")

local function add_ruby_deps_command(client, bufnr)
  local show_ruby_deps = function(opts)
    local params = vim.lsp.util.make_text_document_params()
    local showAll = opts.args == "all"

    client.request("rubyLsp/workspace/dependencies", params, function(error, result)
      if error then
        print("Error showing deps: " .. error)
        return
      end

      local qf_list = {}
      for _, item in ipairs(result) do
        if showAll or item.dependency then
          table.insert(qf_list, {
            text = string.format("%s (%s) - %s", item.name, item.version, item.dependency),
            filename = item.path
          })
        end
      end

      vim.fn.setqflist(qf_list)
      vim.cmd("copen")
    end, bufnr)
  end

  vim.api.nvim_buf_create_user_command(bufnr, "ShowRubyDeps", show_ruby_deps, {
    nargs = "?",
    complete = function()
      return { "all" }
    end
  })
end

local function disable_features_present_in_sorbet(client, bufnr)
  lsp_utils.each_client({ bufnr = bufnr }, function(c)
    if c.name == "sorbet" then
      client.server_capabilities.documentSymbolProvider = false
      return true
    end
  end)
end

local function open_file(command, _ctx)
  local args = command.arguments[1]
  if args and args[1] then
    vim.cmd("edit " .. vim.uri_to_fname(args[1]))
  end
end

return {
  cmd = lsp_utils.asdf_shim("ruby-lsp"),
  filetypes = { "ruby", "eruby" },
  root_markers = { "Gemfile", ".git" },
  cmd_env = {
    RAILS_ENV = "test",
  },
  init_options = {
    formatter = "none",
    linters = {},
    addonSettings = {
      ["Ruby LSP Rails"] = {
        enablePendingMigrationsPrompt = false,
      },
    },
  },
  on_attach = function(client, bufnr)
    client.commands["rubyLsp.openFile"] = open_file
    add_ruby_deps_command(client, bufnr)
    disable_features_present_in_sorbet(client, bufnr)
  end,
}
