local utils = require("plugins.lsp.utils")

local function add_ruby_deps_command(client, bufnr)
  vim.api.nvim_buf_create_user_command(bufnr, "ShowRubyDeps", function(opts)
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
      vim.cmd('copen')
    end, bufnr)
  end,
  { nargs = "?", complete = function() return { "all" } end })
end

local function open_file(command, ctx)
  local args = command.arguments[1]
  if args and args[1] then
    vim.cmd("edit " .. vim.uri_to_fname(args[1]))
  end
end

return {
  server = {
    mason = false,
    cmd = utils.asdf_shim("ruby-lsp")
  },
  setup = function(_, opts)
    opts.on_attach = function(client, buffer)
      client.commands["rubyLsp.openFile"] = open_file
      add_ruby_deps_command(client, buffer)
    end
  end
}
