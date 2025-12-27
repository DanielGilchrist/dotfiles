vim.api.nvim_create_user_command("BundleOpen", function()
  require("custom.bundle_open").open()
end, {})
