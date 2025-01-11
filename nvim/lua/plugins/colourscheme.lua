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
  colourscheme("scottmckendry/cyberdream.nvim", "cyberdream"),
  colourscheme("Shatur/neovim-ayu", "ayu"),
  colourscheme("ray-x/starry.nvim", "starry"),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "catppuccin-mocha",
    },
  },
}
