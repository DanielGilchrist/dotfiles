local is = require("utils.is")
local notify = require("utils.notify")

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

vim.diagnostic.config({
  virtual_text = false,
})

vim.g.autoformat = true

vim.api.nvim_create_autocmd("BufWritePre", {
  group = vim.api.nvim_create_augroup("LspAutoFormat", {}),
  callback = function(ev)
    local buf = ev.buf
    local baf = vim.b[buf].autoformat

    if baf == false or (baf == nil and not vim.g.autoformat) then
      return
    end

    local clients = vim.lsp.get_clients({ bufnr = buf, method = "textDocument/formatting" })
    if #clients > 0 then
      vim.lsp.buf.format({ bufnr = buf })
    end
  end,
})

vim.api.nvim_create_autocmd("LspAttach", {
  callback = function(ev)
    local buf = ev.buf
    local map = function(mode, lhs, rhs, desc)
      vim.keymap.set(mode, lhs, rhs, { buf = buf, desc = desc })
    end

    map("n", "gd", function() Snacks.picker.lsp_definitions() end, "Go to Definition")
    map("n", "gr", function() Snacks.picker.lsp_references() end, "References")
    map("n", "gI", function() Snacks.picker.lsp_implementations() end, "Go to Implementation")
    map("n", "gy", function() Snacks.picker.lsp_type_definitions() end, "Go to Type Definition")
    map("n", "gD", vim.lsp.buf.declaration, "Go to Declaration")
    map("n", "gK", vim.lsp.buf.signature_help, "Signature Help")
    map("i", "<c-k>", vim.lsp.buf.signature_help, "Signature Help")
    map("n", "K", combined_hover, "Hover")

    map("n", "<leader>ca", vim.lsp.buf.code_action, "Code Action")
    map("n", "<leader>cr", vim.lsp.buf.rename, "Rename")
    map("n", "<leader>cc", vim.lsp.codelens.run, "Run Codelens")
    map("n", "<leader>cC", function() vim.lsp.codelens.enable(true) end, "Refresh Codelens")
    map("n", "<leader>cd", vim.diagnostic.open_float, "Line Diagnostics")
    map("n", "<leader>cf", function() vim.lsp.buf.format({ async = true }) end, "Format")

    map("n", "<leader>uf", function()
      vim.g.autoformat = not vim.g.autoformat
      vim.notify("Autoformat (global): " .. (vim.g.autoformat and "enabled" or "disabled"))
    end, "Toggle Autoformat (Global)")

    map("n", "<leader>uF", function()
      vim.b[buf].autoformat = not vim.b[buf].autoformat
      vim.notify("Autoformat (buffer): " .. (vim.b[buf].autoformat and "enabled" or "disabled"))
    end, "Toggle Autoformat (Buffer)")
  end,
})

vim.api.nvim_create_user_command("LspLog", function()
  vim.cmd.edit(vim.lsp.log.get_filename())
end, {})

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
