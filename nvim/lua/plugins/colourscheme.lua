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
      transparent_background = true,
    }
  }),
  colourscheme("folke/tokyonight.nvim", {
    opts = {
      transparent = true,
    }
  }),
  colourscheme("scottmckendry/cyberdream.nvim"),
  colourscheme("Shatur/neovim-ayu"),
  colourscheme("wtfox/jellybeans.nvim", {
    opts = {
      transparent = true,
    }
  }),
  colourscheme("rose-pine/neovim", {
    name = "rose-pine",
    opts = {
      styles = {
        transparency = true,
      }
    }
  }),
  colourscheme("neanias/everforest-nvim", {
    config = function()
      require("everforest").setup({
        background = "hard",
        transparent_background_level = 2,
      })
    end,
  }),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "jellybeans",
    },
  },
}
