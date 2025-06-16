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
      preview = {
        vertical = true
      }
    },
    view_options = {
      show_hidden = true,
    }
  },
  keys = {
    {
      "<leader>e",
      function()
        -- TODO: Look into allowing toggle_float to accept opts just like open_float
        -- API Docs: https://github.com/stevearc/oil.nvim/blob/master/doc/api.md#open_floatdir-opts-cb
        -- toggle_float def: https://github.com/stevearc/oil.nvim/blob/08c2bce8b00fd780fb7999dbffdf7cd174e896fb/lua/oil/init.lua#L345-L351
        --
        -- require("oil").toggle_float(nil, {
        --   preview = {
        --     vertical = true
        --   }
        -- })

        local oil = require("oil")

        if vim.w.is_oil_win then
          return oil.close()
        end

        oil.open_float(nil, {
          preview = {
            vertical = true
          }
        })
      end,
      desc = "Explore Files (Oil)",
    }
  }
}
