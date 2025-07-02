return {
  "MagicDuck/grug-far.nvim",
  keys = {
    {
      "<leader>br",
      function()
        require('grug-far').with_visual_selection({
          prefills = {
            paths = vim.fn.expand("%"),
          },
        })
      end,
      mode = "v",
      desc = "Search and replace (Current file)"
    }
  },
  opts = {
    prefills = {
      flags = "--hidden",
      filesFilter = "!.git"
    }
  }
}
