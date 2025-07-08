local function disable_format(client)
  client.server_capabilities.documentFormattingProvider = false
  client.server_capabilities.documentRangeFormattingProvider = false
end

local function asdf_shim(command)
  return { vim.fn.expand("~/.asdf/shims/" .. command) }
end

local function gem_available(gem_name)
  local result = vim.fn.system("bundle show " .. gem_name .. " 2>/dev/null")
  return vim.v.shell_error == 0
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

local function gopls_organise_imports_and_format_on_save(client, bufnr)
  vim.api.nvim_create_autocmd("BufWritePre", {
    buffer = bufnr,
    callback = function()
      -- Organise imports
      local params = vim.lsp.util.make_range_params(nil, client.offset_encoding)
      params = vim.tbl_extend("force", params, {
        context = { only = { "source.organizeImports" } }
      })

      local result = vim.lsp.buf_request_sync(bufnr, "textDocument/codeAction", params, 3000)
      for _, res in pairs(result or {}) do
        for _, r in pairs(res.result or {}) do
          if r.edit then
            vim.lsp.util.apply_workspace_edit(r.edit, client.offset_encoding)
          elseif r.command then
            local ctx = { bufnr = bufnr }
            client:exec_cmd(r.command, ctx)
          end
        end
      end

      -- Format file
      vim.lsp.buf.format({ async = false, bufnr = bufnr })
    end,
    group = vim.api.nvim_create_augroup("GoFormat" .. bufnr, { clear = true }),
  })
end

local function find_crystalline()
  local paths = {
    "/opt/homebrew/bin/crystalline", -- Apple Silicon Mac
    "/usr/local/bin/crystalline",    -- Intel Mac/Linux
  }

  for _, path in ipairs(paths) do
    if vim.fn.executable(path) == 1 then
      return path
    end
  end

  return "crystalline"
end

return {
  "neovim/nvim-lspconfig",
  opts = function(_, opts)
    opts.diagnostics.virtual_text = false
    opts.codelens = {
      enabled = true
    }
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
      gopls = function(_, gopls_opts)
        gopls_opts.settings = {
          gopls = {
            gofumpt = true,
            staticcheck = true,
            analyses = {
              unusedparams = true,
            },
            importOrganization = true,
          },
        }

        gopls_opts.on_attach = function(client, bufnr)
          gopls_organise_imports_and_format_on_save(client, bufnr)
        end
      end,
      rubocop = function(_, rubocop_opts)
        rubocop_opts.autostart = gem_available("rubocop")
      end,
      ruby_lsp = function(_, rlsp_opts)
        rlsp_opts.on_attach = function(client, buffer)
          client.commands["rubyLsp.openFile"] = function(command, ctx)
            local args = command.arguments[1]
            if args and args[1] then
              vim.cmd("edit " .. vim.uri_to_fname(args[1]))
            end
          end

          add_ruby_deps_command(client, buffer)
        end
      end,
      sorbet = function(_, sorbet_opts)
        sorbet_opts.autostart = gem_available("sorbet")
      end,
    }
    opts.servers = {
      crystalline = {
        mason = false,
        cmd = { find_crystalline() },
      },
      -- flow = {},
      gopls = {}, -- Settings will be added via the setup function above
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
