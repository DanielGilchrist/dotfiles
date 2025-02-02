local function colourscheme(source, name, opts)
  opts = opts == nil and {} or opts

  return {
    source,
    name = name,
    opts = opts,
    event = "User LazyColorscheme",
  }
end

return {
  colourscheme("catppuccin/nvim", "catppuccin", {
    transparent_background = true,
  }),
  colourscheme("folke/tokyonight.nvim", "tokyonight", {
    transparent = true,
  }),
  colourscheme("scottmckendry/cyberdream.nvim", "cyberdream"),
  colourscheme("Shatur/neovim-ayu", "ayu"),
  colourscheme("ray-x/starry.nvim", "starry"),
  colourscheme("wtfox/jellybeans.nvim", "jellybeans"),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "tokyonight-night",
    },
  },
}
