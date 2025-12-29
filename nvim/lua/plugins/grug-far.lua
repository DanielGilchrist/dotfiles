local grug_far = function()
  return require("grug-far")
end

return {
  "MagicDuck/grug-far.nvim",
  keys = {
    {
      "<leader>sr",
      function()
        grug_far().open({
          transient = true,
        })
      end,
      mode = { "n", "x" },
      desc = "Search and Replace",
    },
    {
      "<leader>br",
      function()
        grug_far().with_visual_selection({
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
