local pack = require("utils.pack")

pack.later({
  "https://github.com/folke/noice.nvim",
  "https://github.com/MunifTanjim/nui.nvim",
}, function()
  require("noice").setup({
    lsp = {
      hover = {
        silent = true,
      },
    },
  })
end)
