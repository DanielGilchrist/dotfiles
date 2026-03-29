local is = require("utils.is")
local notify = require("utils.notify")

-- Diagnostics
vim.diagnostic.config({
  virtual_text = false,
})

-- Smart hover: uses default hover for single client (noice-compatible),
-- falls back to combined hover when multiple clients provide hover
local function smart_hover()
  local clients = vim.lsp.get_clients({ bufnr = 0 })
  local hover_clients = vim.iter(clients)
    :filter(function(c) return c:supports_method("textDocument/hover", 0) end)
    :totable()

  if #hover_clients <= 1 then
    return vim.lsp.buf.hover()
  end

  -- Multiple clients: merge results
  local results = {}
  local pending = #hover_clients

  for _, client in ipairs(hover_clients) do
    local params = vim.lsp.util.make_position_params(0, client.offset_encoding)
    client:request("textDocument/hover", params, function(err, result)
      if not err and result and result.contents and result.contents.value and result.contents.value ~= "" then
        table.insert(results, { name = client.name, contents = result.contents.value })
      end
      pending = pending - 1
      if pending == 0 and #results > 0 then
        table.sort(results, function(a, b) return #a.contents < #b.contents end)
        local lines = {}
        for i, item in ipairs(results) do
          if i > 1 then vim.list_extend(lines, { "", "---", "" }) end
          vim.list_extend(lines, { "**" .. item.name .. "**", "" })
          vim.list_extend(lines, vim.split(item.contents, "\n"))
        end
        vim.lsp.util.open_floating_preview(lines, "markdown", {
          border = "rounded",
          focusable = true,
          focus_id = "hover",
        })
      end
    end)
  end
end

-- LSP keymaps on attach
vim.api.nvim_create_autocmd("LspAttach", {
  callback = function(ev)
    local buf = ev.buf
    local map = function(mode, lhs, rhs, desc)
      vim.keymap.set(mode, lhs, rhs, { buf = buf, desc = desc })
    end

    -- Navigation
    map("n", "gd", vim.lsp.buf.definition, "Go to Definition")
    map("n", "gr", vim.lsp.buf.references, "References")
    map("n", "gI", vim.lsp.buf.implementation, "Go to Implementation")
    map("n", "gy", vim.lsp.buf.type_definition, "Go to Type Definition")
    map("n", "gD", vim.lsp.buf.declaration, "Go to Declaration")
    map("n", "gK", vim.lsp.buf.signature_help, "Signature Help")
    map("i", "<c-k>", vim.lsp.buf.signature_help, "Signature Help")
    map("n", "K", smart_hover, "Hover")

    -- Code actions (leader-c)
    map("n", "<leader>ca", vim.lsp.buf.code_action, "Code Action")
    map("n", "<leader>cr", vim.lsp.buf.rename, "Rename")
    map("n", "<leader>cc", vim.lsp.codelens.run, "Run Codelens")
    map("n", "<leader>cC", vim.lsp.codelens.refresh, "Refresh Codelens")
    map("n", "<leader>cd", vim.diagnostic.open_float, "Line Diagnostics")
    map("n", "<leader>cf", function() vim.lsp.buf.format({ async = true }) end, "Format")
  end,
})


vim.api.nvim_create_user_command("LspLog", function()
  vim.cmd.edit(vim.lsp.get_log_path())
end, {})

-- Enable LSP servers (configs auto-loaded from lsp/ directory)
vim.lsp.enable({
  "arduino_language_server",
  "crystalline",
  "gopls",
  "kotlin_lsp",
  "lua_ls",
  "rubocop",
  "ruby_lsp",
  "rust_analyzer",
  "sorbet",
  "sourcekit",
  "ts_ls",
  "yamlls",
  "zls",
})
