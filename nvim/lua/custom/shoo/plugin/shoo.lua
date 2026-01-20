local shoo = function()
  return require("custom.shoo")
end

vim.api.nvim_create_user_command("GHPurgeForce", function()
  shoo().purge(true)
end, {})
