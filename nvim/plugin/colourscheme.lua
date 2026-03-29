local pack = require("utils.pack")
pack.add({
  "https://github.com/RRethy/base16-nvim",
  { src = "https://github.com/everviolet/nvim", name = "evergarden" },
})

require("evergarden").setup({
  theme = {
    variant = "winter",
    accent = "green",
  },
  editor = {
    transparent_background = false,
    sign = { color = "none" },
    float = {
      color = "mantle",
      solid_border = false,
    },
    completion = {
      color = "surface0",
    },
  },
})

vim.cmd.colorscheme("evergarden")
