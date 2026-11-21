local path = require("utils.path")

---@param opts? snacks.terminal.Opts
local function open_terminal(opts)
  local default_opts = {
    win = {
      position = "right"
    }
  }

  local merged_opts = vim.tbl_extend("force", default_opts, opts or {})

  Snacks.terminal(nil, merged_opts)
end

---@param buf integer
---@return snacks.win?
local function find_snacks_terminal(buf)
  for _, terminal in pairs(Snacks.terminal.list()) do
    if terminal.buf == buf then
      return terminal
    end
  end

  return nil
end

---@type table<snacks.win, Partial<snacks.win.Config>>
local zoom_restore = setmetatable({}, { __mode = "k" })

local function toggle_terminal_zoom()
  local buf = vim.api.nvim_get_current_buf()

  local snacks = find_snacks_terminal(buf)
  if snacks then
    if snacks:valid() then
      snacks:hide()
    end

    if snacks.opts.position == "float" then
      local restore = zoom_restore[snacks] or {}
      snacks.opts.position = restore.position or "right"
      snacks.opts.height = restore.height
      snacks.opts.width = restore.width
    else
      zoom_restore[snacks] = { position = snacks.opts.position, height = snacks.opts.height, width = snacks.opts.width }
      snacks.opts.position = "float"
      snacks.opts.height = 0.9
      snacks.opts.width = 0.9
    end

    vim.schedule(function ()
      snacks:show()
    end)
  end
end

return {
  keys = {
    {
      "<leader>fT",
      function ()
        open_terminal()
      end,
      desc = "Terminal (cwd)",
      mode = "n"
    },
    {
      "<leader>ft",
      function ()
        open_terminal({ cwd = path.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = "n"
    },
    {
      "<c-/>",
      function ()
        Snacks.terminal.toggle(nil, { cwd = path.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = { "n", "t" }
    },
    {
      "<c-,>",
      toggle_terminal_zoom,
      desc = "Toggle Terminal Zoom",
      mode = { "n", "t", "i", "x" }
    }
  }
}
