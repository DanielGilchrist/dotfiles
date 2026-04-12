return {
  "Wansmer/treesj",
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  opts = {
    use_default_keymaps = false,
    max_join_length = 144,
  },
  keys = {
    {
      "<leader>ct",
      function() require("treesj").toggle({ split = { recursive = true } }) end,
      desc = "treesj toggle",
    },
  },
}
