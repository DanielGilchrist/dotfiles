local pack = require("utils.pack")

pack.later({ "https://github.com/lewis6991/gitsigns.nvim" }, function()
  require("gitsigns").setup({
    current_line_blame = true,
  })
end)
