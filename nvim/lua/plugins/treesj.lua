return {
  "Wansmer/treesj",
  keys = {
    {
      "<leader>ct",
      function()
        require("treesj").toggle({ split = { recursive = true } })
      end,
      desc = "treesj toggle"
    },
  },
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  opts = {
    use_default_keymaps = false,
  }
}
