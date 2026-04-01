local pack = require("utils.pack")

pack.add({ "https://github.com/nvim-lualine/lualine.nvim" })

require("lualine").setup({
  tabline = {
    lualine_a = {},
    lualine_b = { { "buffers", symbols = { alternate_file = "" } } },
    lualine_z = {},
  },
  extensions = {
    "oil",
  },
})
