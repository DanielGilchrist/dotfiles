local pack = require("utils.pack")

pack.later({ "https://github.com/folke/persistence.nvim" }, function()
  require("persistence").setup()
end)
