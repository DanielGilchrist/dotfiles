local yank_test_line = function()
  return require("custom.yank_test_line")
end

vim.api.nvim_create_user_command("Ytest", function()
  yank_test_line().yank()
end, {})

vim.api.nvim_create_user_command("Ytestn", function()
  yank_test_line().yank_with_number()
end, {})
