local pack = require("utils.pack")

pack.add({ "https://github.com/folke/lazydev.nvim" })

require("lazydev").setup({
  library = {
    { path = "${3rd}/luv/library", words = { "vim%.uv" } },
    "snacks.nvim",
  },
})
