vim.keymap.set("n", "<leader>cD", function()
  require("custom.lint_disable").disable_lint()
end, { desc = "Disable lint rule inline" })
