vim.api.nvim_create_user_command("BundleOpen", function()
  require("bundle_open").open()
end, {})
