local pack = require("utils.pack")

pack.later({ "https://github.com/nvim-mini/mini.surround" }, function()
  require("mini.surround").setup()
end)
