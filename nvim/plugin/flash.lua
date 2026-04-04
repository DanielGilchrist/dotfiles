local pack = require("utils.pack")

pack.later({ "https://github.com/folke/flash.nvim" }, function()
  require("flash").setup()

  vim.keymap.set({ "n", "x", "o" }, "gs", function() require("flash").jump() end, { desc = "Flash" })
  vim.keymap.set({ "n", "x", "o" }, "gS", function() require("flash").treesitter() end, { desc = "Flash Treesitter" })
  vim.keymap.set("o", "r", function() require("flash").remote() end, { desc = "Remote Flash" })
  vim.keymap.set({ "o", "x" }, "R", function() require("flash").treesitter_search() end, { desc = "Treesitter Search" })
end)
