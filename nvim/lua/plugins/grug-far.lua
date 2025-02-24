return {
  "MagicDuck/grug-far.nvim",
  keys = {
    {
      "<leader>cr",
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
  -- opts = {
  --   prefills = {
  --     flags = "--hidden --glob '!.git'"
  --   }
  -- }
}
