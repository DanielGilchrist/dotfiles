local loader = require("utils.plugin_loader")
local is = require("utils.is")
local notify = require("utils.notify")

local setup = {}
local servers = {}

loader.each_config("plugins/lsp/servers", function(config, name)
  local setup_config = config.setup
  if setup_config then
    if is.table(setup_config) then
      local setup_table = setup_config
      setup_config = function(_, opts)
        for k, v in pairs(setup_table) do
          opts[k] = v
        end
      end
    end

    setup[name] = setup_config
  end

  servers[name] = config.server or {}
end)

-- TODO: Extract this function to a separate file and avoid nesting functions
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

  local extract_contents = function(result)
    if is.empty(result) then
      return nil
    end

    local value = result.contents.value

    if is.empty(value) then
      return nil
    end

    return value
  end

  local handle_client = function(client)
    handle_count = handle_count + 1

    local params = vim.lsp.util.make_position_params(0, client.offset_encoding)

    local handle_hover = function(err, result)
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
