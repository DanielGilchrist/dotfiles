-- Autocmds are automatically loaded on the VeryLazy event
-- Default autocmds that are always set: https://github.com/LazyVim/LazyVim/blob/main/lua/lazyvim/config/autocmds.lua
-- Add any additional autocmds here

local create_autocmd = vim.api.nvim_create_autocmd

vim.api.nvim_create_user_command("Dashboard", function()
  Snacks.dashboard()
end, {})

-- nuke trailing whitespace on save
create_autocmd("BufWritePre", {
  pattern = "*",
  command = [[%s/\s\+$//e]],
})

-- show dashboard if no files are open
create_autocmd("BufDelete", {
  group = vim.api.nvim_create_augroup("bufdelpost_autocmd", {}),
  desc = "BufDeletePost User autocmd",
  callback = function()
    vim.schedule(function()
      vim.api.nvim_exec_autocmds("User", {
        pattern = "BufDeletePost",
      })
    end)
  end,
})

create_autocmd("User", {
  pattern = "BufDeletePost",
  group = vim.api.nvim_create_augroup("dashboard_delete_buffers", {}),
  desc = "Open Dashboard when no available buffers",
  callback = function(event)
    local no_file_buffers_open = function()
      local deleted_name = vim.api.nvim_buf_get_name(event.buf)
      if deleted_name ~= "" then
        return false
      end

      local deleted_file_type = vim.api.nvim_get_option_value("filetype", { buf = event.buf })
      if deleted_file_type ~= "" then
        return false
      end

      local deleted_buf_type = vim.api.nvim_get_option_value("buftype", { buf = event.buf })
      if deleted_buf_type ~= "" then
        return false
      end

      return true
    end

    if no_file_buffers_open() then
      Snacks.dashboard()
    end
  end,
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
