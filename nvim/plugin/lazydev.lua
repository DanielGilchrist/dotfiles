local pack = require("utils.pack")

pack.later({
  "https://github.com/folke/lazydev.nvim",
  "https://github.com/DrKJeff16/wezterm-types",
}, function()
  require("lazydev").setup({
    library = {
      { path = "${3rd}/luv/library", words = { "vim%.uv" } },
      { path = "wezterm-types", words = { "wezterm" } },
      "snacks.nvim",
    },
  })
end)
