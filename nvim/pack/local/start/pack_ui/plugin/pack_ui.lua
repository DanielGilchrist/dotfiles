vim.api.nvim_create_user_command("Pack", function()
  require("pack_ui").open()
end, {})

vim.keymap.set("n", "<leader>p", function()
  require("pack_ui").open()
end, { desc = "Pack" })
