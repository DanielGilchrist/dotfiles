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

local function combined_hover()
  local clients = vim.lsp.get_clients({ bufnr = 0 })
  local results = {}
  local handle_count = 0

  local open_hover_window = function()
    table.sort(results, function(a, b)
      return #a.contents < #b.contents
    end)

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

  local handle_client = function(client)
    handle_count = handle_count + 1

    local params = vim.lsp.util.make_position_params(0, client.offset_encoding)

    local handle_hover = function(err, result)
      if not err and result and result.contents then
        local value = type(result.contents) == "string" and result.contents or result.contents.value
        table.insert(results, { name = client.name, contents = value })
      end

      handle_count = handle_count - 1

      if handle_count ~= 0 then
        return
      end

      if #results == 0 then
        return
      end

      open_hover_window()
    end

    client.request("textDocument/hover", params, handle_hover)
  end

  for _, client in ipairs(clients) do
    if client.supports_method("textDocument/hover", 0) then
      handle_client(client)
    end
  end
end

return {
  "neovim/nvim-lspconfig",
  opts = {
    setup = setup,
    servers = vim.tbl_extend("force", servers, {
      ["*"] = {
        keys = {
          { "K", combined_hover, desc = "Hover" },
        },
      }
    }),
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
