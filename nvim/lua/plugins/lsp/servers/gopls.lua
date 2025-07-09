local function organise_imports_and_format_on_save(client, bufnr)
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

return {
  setup = function(_, opts)
    opts.settings = {
      gopls = {
        gofumpt = true,
        staticcheck = true,
        analyses = {
          unusedparams = true,
        },
        importOrganization = true,
      },
    }

    opts.on_attach = function(client, bufnr)
      organise_imports_and_format_on_save(client, bufnr)
    end
  end
}
