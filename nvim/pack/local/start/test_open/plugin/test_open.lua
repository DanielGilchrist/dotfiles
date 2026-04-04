vim.api.nvim_create_user_command("TestOpen", function()
  require("test_open").open_test()
end, {})
