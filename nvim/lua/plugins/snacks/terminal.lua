local function open_terminal(opts)
  opts = opts == nil and {} or opts

  local default_opts = {
    win = {
      position = "right"
    }
  }

  local merged_opts = vim.tbl_extend("force", default_opts, opts)

  Snacks.terminal(nil, merged_opts)
end

local function find_sidekick_terminal(buf)
  local ok, State = pcall(require, "sidekick.cli.state")
  if not ok then
    return nil
  end

  local states = State.get({ attached = true, terminal = true })
  for _, state in ipairs(states) do
    local terminal = state.terminal
    if terminal and terminal.buf == buf then
      return terminal
    end
  end

  return nil
end

local function find_snacks_terminal(buf)
  for _, terminal in pairs(Snacks.terminal.list()) do
    if terminal.buf == buf then
      return terminal
    end
  end

  return nil
end

local function toggle_terminal_zoom()
  local buf = vim.api.nvim_get_current_buf()

  local sidekick = find_sidekick_terminal(buf)
  if sidekick then
    if sidekick:is_open() then
      sidekick:hide()
    end

    if sidekick.opts.layout == "float" then
      sidekick.opts.layout = "right"
    else
      sidekick.opts.layout = "float"
    end

    vim.schedule(function()
      sidekick:show()
      sidekick:focus()
    end)
    return
  end

  local snacks = find_snacks_terminal(buf)
  if snacks then
    if snacks:valid() then
      snacks:hide()
    end

    if snacks.opts.position == "float" then
      snacks.opts.position = snacks._zoom_restore_position or "right"
      snacks.opts.height = snacks._zoom_restore_height
      snacks.opts.width = snacks._zoom_restore_width
    else
      snacks._zoom_restore_position = snacks.opts.position
      snacks._zoom_restore_height = snacks.opts.height
      snacks._zoom_restore_width = snacks.opts.width
      snacks.opts.position = "float"
      snacks.opts.height = 0.9
      snacks.opts.width = 0.9
    end

    vim.schedule(function()
      snacks:show()
    end)
  end
end

return {
  keys = {
    {
      "<leader>fT",
      function()
        open_terminal()
      end,
      desc = "Terminal (cwd)",
      mode = "n"
    },
    {
      "<leader>ft",
      function()
        open_terminal({ cwd = LazyVim.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = "n"
    },
    {
      "<c-/>",
      function()
        open_terminal({ cwd = LazyVim.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = "n"
    },
    {
      "<c-,>",
      toggle_terminal_zoom,
      desc = "Toggle Terminal Zoom",
      mode = { "n", "t", "i", "x" },
    },
  },
}
