return {
  "stevearc/oil.nvim",
  dependencies = {
    {
      "echasnovski/mini.icons",
      opts = {},
    },
  },
  opts = {
    float = {
      padding = 5,
    },
    view_options = {
      show_hidden = true,
    }
  },
  keys = {
    {
      "<leader>e",
      function()
        require("oil").open_float()
      end,
      desc = "Explore Files (Oil)",
    }
  }
}
