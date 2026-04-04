local pack = require("utils.pack")

pack.later({ "https://github.com/rachartier/tiny-inline-diagnostic.nvim" }, function()
  require("tiny-inline-diagnostic").setup()
end)
