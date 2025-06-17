transparent = false

local function colourscheme(source, options)
  options = options == nil and {} or options

  return vim.tbl_extend(
    "error",
    {
      source,
      event = "User LazyColorscheme"
    },
    options
  )
end

return {
  colourscheme("catppuccin/nvim", {
    name = "catppuccin",
    opts = {
      transparent_background = transparent,
    }
  }),
  colourscheme("folke/tokyonight.nvim", {
    opts = {
      transparent = transparent,
    }
  }),
  colourscheme("ellisonleao/gruvbox.nvim", {
    opts = {
      transparent_mode = transparent
    }
  }),
  colourscheme("scottmckendry/cyberdream.nvim"),
  colourscheme("Shatur/neovim-ayu"),
  colourscheme("wtfox/jellybeans.nvim", {
    opts = {
      transparent = transparent,
    }
  }),
  colourscheme("neanias/everforest-nvim", {
    config = function()
      require("everforest").setup({
        background = "hard",
        -- transparent_background_level = 2,
      })
    end,
  }),
  colourscheme("EdenEast/nightfox.nvim", {
    opts = {
      options = {
        transparent = transparent
      }
    }
  }),
  colourscheme("daneofmanythings/chalktone.nvim", {
    opts = {
      theme = "default"
    }
  }),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "chalktone",
    },
  },
}
