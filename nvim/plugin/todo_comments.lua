local pack = require("utils.pack")

pack.later({ "https://github.com/folke/todo-comments.nvim" }, function()
  require("todo-comments").setup()
end)
