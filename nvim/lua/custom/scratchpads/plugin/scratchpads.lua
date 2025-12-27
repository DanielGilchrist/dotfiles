local scratchpads = function()
  return require("custom.scratchpads")
end

vim.api.nvim_create_user_command("ScratchNew", function()
  scratchpads().new()
end, {})

vim.api.nvim_create_user_command("ScratchOpen", function()
  scratchpads().open()
end, {})

vim.api.nvim_create_user_command("ScratchRename", function()
  scratchpads().rename()
end, {})

vim.api.nvim_create_user_command("ScratchRemove", function()
  scratchpads().remove()
end, {})
