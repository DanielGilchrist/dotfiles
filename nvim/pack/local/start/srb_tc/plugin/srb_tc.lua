vim.api.nvim_create_user_command("SrbTc", function()
  require("srb_tc").run()
end, {})
