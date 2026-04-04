local pack = require("utils.pack")

pack.later({ "https://github.com/Wansmer/treesj" }, function()
  require("treesj").setup({
    use_default_keymaps = false,
    max_join_length = 144,
  })

  vim.keymap.set("n", "<leader>ct", function()
    require("treesj").toggle({ split = { recursive = true } })
  end, { desc = "treesj toggle" })
end)
