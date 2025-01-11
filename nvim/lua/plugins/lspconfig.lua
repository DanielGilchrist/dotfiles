local function disable_format(client)
  client.server_capabilities.documentFormattingProvider = false
  client.server_capabilities.documentRangeFormattingProvider = false
end

local function asdf_shim(command)
  return { vim.fn.expand("~/.asdf/shims/" .. command) }
end

-- https://shopify.github.io/ruby-lsp/editors.html#additional-setup-optional
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

return {
  "neovim/nvim-lspconfig",
  opts = function(_, opts)
    opts.inlay_hints = {
      enabled = false,
    }

    opts.setup = {
      -- TODO: Can be re-enabled if the following issue is ever resolved: https://github.com/elbywan/crystalline/issues/41
      crystalline = function(_, cr_opts)
        cr_opts.on_attach = function(client)
          disable_format(client)
        end
      end,
      ruby_lsp = function(_, rlsp_opts)
        rlsp_opts.on_attach = function(client, buffer)
          add_ruby_deps_command(client, buffer)
        end
      end,
    }

    opts.servers = {
      crystalline = {
        mason = false,
        cmd = { "/usr/local/bin/crystalline" },
      },
      flow = {},
      gopls = {},
      rubocop = {
        single_file_support = false,
        mason = false,
        cmd = { "bundle", "exec", "rubocop", "--lsp" },
      },
      ruby_lsp = {
        mason = false,
        cmd = asdf_shim("ruby-lsp")
      },
      sorbet = {
        single_file_support = false,
        mason = false,
        cmd = { "bundle", "exec", "srb", "tc", "--lsp" },
      },
      yamlls = {
        settings = {
          yaml = {
            format = {
              enable = false,
            }
          }
        }
      },
      zls = {
        mason = false,
      }
    }
  end,
}
