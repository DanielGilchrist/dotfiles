-- Autocmds are automatically loaded on the VeryLazy event
-- Default autocmds that are always set: https://github.com/LazyVim/LazyVim/blob/main/lua/lazyvim/config/autocmds.lua
-- Add any additional autocmds here

local create_autocmd = vim.api.nvim_create_autocmd

vim.api.nvim_create_user_command("Dashboard", function()
  Snacks.dashboard()
end, {})

vim.api.nvim_create_user_command("Clear", function()
  vim.cmd("%bd")
  vim.cmd("Dashboard")
end, {})

-- nuke trailing whitespace on save
create_autocmd("BufWritePre", {
  pattern = "*",
  command = [[%s/\s\+$//e]],
})

-- Show absolute numbers in insert mode
create_autocmd("InsertEnter", {
  callback = function()
    vim.opt.relativenumber = false
  end
})

create_autocmd("InsertLeave", {
  callback = function()
    vim.opt.relativenumber = true
  end
})
