local pack = require("utils.pack")

pack.later({ "https://github.com/MagicDuck/grug-far.nvim" }, function()
  require("grug-far").setup({
    startInInsertMode = false,
    prefills = {
      flags = "--hidden",
      filesFilter = "!.git",
    },
  })

  local grug_far = function() return require("grug-far") end

  vim.keymap.set({ "n", "x" }, "<leader>sr", function()
    grug_far().open({ transient = true })
  end, { desc = "Search and Replace" })

  vim.keymap.set("v", "<leader>br", function()
    grug_far().with_visual_selection({
      startCursorRow = 2,
      prefills = { paths = vim.fn.expand("%") },
    })
  end, { desc = "Search and replace (Current file)" })
end)
