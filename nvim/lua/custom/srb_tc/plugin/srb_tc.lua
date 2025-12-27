vim.api.nvim_create_user_command("SrbTc", function()
  require("custom.srb_tc").run()
end, {})
