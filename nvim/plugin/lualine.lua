local pack = require("utils.pack")

pack.add({ "https://github.com/nvim-lualine/lualine.nvim" })

require("lualine").setup({
  extensions = {
    "oil",
  },
})
