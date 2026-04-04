local pack = require("utils.pack")

pack.later({ "https://github.com/folke/lazydev.nvim" }, function()
  require("lazydev").setup({
    library = {
      { path = "${3rd}/luv/library", words = { "vim%.uv" } },
      "snacks.nvim",
    },
  })
end)
