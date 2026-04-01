local pack = require("utils.pack")

pack.add({ "https://github.com/nvim-treesitter/nvim-treesitter-textobjects" })

require("nvim-treesitter-textobjects").setup()

local move = require("nvim-treesitter-textobjects.move")

local function map(keys, fn, desc)
  vim.keymap.set({ "n", "x", "o" }, keys, fn, { desc = desc })
end

map("]f", function() move.goto_next_start("@function.outer") end, "Next function start")
map("]F", function() move.goto_next_end("@function.outer") end, "Next function end")
map("[f", function() move.goto_previous_start("@function.outer") end, "Previous function start")
map("[F", function() move.goto_previous_end("@function.outer") end, "Previous function end")

map("]c", function()
  if vim.wo.diff then vim.cmd("normal! ]c") else move.goto_next_start("@class.outer") end
end, "Next class start")
map("]C", function() move.goto_next_end("@class.outer") end, "Next class end")
map("[c", function()
  if vim.wo.diff then vim.cmd("normal! [c") else move.goto_previous_start("@class.outer") end
end, "Previous class start")
map("[C", function() move.goto_previous_end("@class.outer") end, "Previous class end")

map("]a", function() move.goto_next_start("@parameter.inner") end, "Next argument")
map("]A", function() move.goto_next_end("@parameter.inner") end, "Next argument end")
map("[a", function() move.goto_previous_start("@parameter.inner") end, "Previous argument")
map("[A", function() move.goto_previous_end("@parameter.inner") end, "Previous argument end")
