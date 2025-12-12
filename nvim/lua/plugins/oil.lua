return {
  "stevearc/oil.nvim",
  dependencies = {
    {
      "nvim-mini/mini.icons",
      opts = {},
    },
  },
  opts = {
    keymaps = {
      ["q"] = { "actions.close", mode = "n" },
      ["<C-d>"] = { "actions.preview_scroll_down", mode = "n" },
      ["<C-u>"] = { "actions.preview_scroll_up", mode = "n" }
    },
    float = {
      border = "rounded",
      padding = 3,
      preview = {
        vertical = true
      }
    },
    preview_win = {
      preview_method = "load",
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
