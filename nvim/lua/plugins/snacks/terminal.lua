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
  },
}
