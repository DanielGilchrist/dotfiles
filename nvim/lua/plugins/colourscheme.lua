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
  colourscheme("RRethy/base16-nvim"),
  colourscheme("scottmckendry/cyberdream.nvim"),
  colourscheme("wtfox/jellybeans.nvim", {
    opts = {
      transparent = transparent,
    }
  }),
  colourscheme("EdenEast/nightfox.nvim", {
    opts = {
      options = {
        transparent = transparent
      }
    }
  }),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "base16-gruvbox-material-dark-hard",
    },
  },
}
