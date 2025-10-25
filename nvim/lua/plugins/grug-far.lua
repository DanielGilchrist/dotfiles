return {
  "MagicDuck/grug-far.nvim",
  keys = {
    {
      "<leader>sr",
      function()
        local grug = require("grug-far")
        grug.open({
          transient = true,
        })
      end,
      mode = { "n", "x" },
      desc = "Search and Replace",
    },
    {
      "<leader>br",
      function()
        require('grug-far').with_visual_selection({
          startCursorRow = 2,
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
    startInInsertMode = false,
    prefills = {
      flags = "--hidden",
      filesFilter = "!.git"
    }
  }
}
