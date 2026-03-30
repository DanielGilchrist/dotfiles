local is = require("utils.is")
local notify = require("utils.notify")

-- Diagnostics
vim.diagnostic.config({
  virtual_text = false,
})

-- Combined hover (merges hover results from all attached LSP clients)
local function combined_hover()
  local clients = vim.lsp.get_clients({ bufnr = 0 })
  local results = {}
  local handle_count = 0

  local function open_hover_window()
    table.sort(results, function(a, b) return #a.contents < #b.contents end)

    local lines = {}

    for i, item in ipairs(results) do
      if i > 1 then
        table.insert(lines, "")
        table.insert(lines, "---")
        table.insert(lines, "")
      end

      if #results > 1 then
        table.insert(lines, "**" .. item.name .. "**")
        table.insert(lines, "")
      end

      vim.list_extend(lines, vim.split(item.contents, "\n"))
    end

    vim.lsp.util.open_floating_preview(
      lines,
      "markdown",
      {
        border = "rounded",
        focusable = true,
        focus_id = "hover",
      }
    )
  end

  local function extract_contents(result)
    if is.empty(result) then
      return nil
    end

    local value = result.contents.value

    if is.empty(value) then
      return nil
    end

    return value
  end

  for _, client in ipairs(clients) do
    if client:supports_method("textDocument/hover", 0) then
      handle_count = handle_count + 1

      local params = vim.lsp.util.make_position_params(0, client.offset_encoding)

      client:request("textDocument/hover", params, function(err, result)
        if is.not_empty(err) then
          return notify.error(vim.inspect(err))
        end

        local contents = extract_contents(result)

        if contents then
          table.insert(results, { name = client.name, contents = contents })
        end

        handle_count = handle_count - 1

        if handle_count ~= 0 then
          return
        end

        if is.empty(results) then
          return
        end

        open_hover_window()
      end)
    end
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
    map("n", "K", combined_hover, "Hover")

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
