return {
  "nvim-treesitter/nvim-treesitter-textobjects",
  branch = "main",
  event = "BufReadPost",
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  config = function()
    require("nvim-treesitter-textobjects").setup({ move = { set_jumps = true } })

    local move = require("nvim-treesitter-textobjects.move")
    local modes = { "n", "x", "o" }

    vim.keymap.set(modes, "]f", function() move.goto_next_start("@function.outer") end, { desc = "Next function start" })
    vim.keymap.set(modes, "]F", function() move.goto_next_end("@function.outer") end, { desc = "Next function end" })
    vim.keymap.set(modes, "[f", function() move.goto_previous_start("@function.outer") end, { desc = "Previous function start" })
    vim.keymap.set(modes, "[F", function() move.goto_previous_end("@function.outer") end, { desc = "Previous function end" })
  end,
}
