return {
  "stevearc/oil.nvim",
  dependencies = {
    {
      "echasnovski/mini.icons",
      opts = {},
    },
  },
  opts = {
    keymaps = {
      ["q"] = { "actions.close", mode = "n" }
    },
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
        require("oil").toggle_float()
      end,
      desc = "Explore Files (Oil)",
    }
  }
}
