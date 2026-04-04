local shoo = function()
  return require("shoo")
end

vim.api.nvim_create_user_command("GHPurgeForce", function()
  shoo().purge(true)
end, {})
