-- Options are automatically loaded before lazy.nvim startup
-- Default options that are always set: https://github.com/LazyVim/LazyVim/blob/main/lua/lazyvim/config/options.lua
-- Add any additional options here

-- vim.lsp.set_log_level("debug")

if vim.g.neovide then
  vim.o.guifont = "JetBrainsMono NF:h14"

  -- NOTE: Transparency doesn't currently work on macos fullscreen
  -- vim.g.neovide_opacity = 0.8
  -- vim.g.neovide_normal_opacity = 0.8

  vim.g.neovide_fullscreen = true
  vim.g.neovide_confirm_quit = true
  vim.g.neovide_hide_mouse_when_typing = true
  vim.g.neovide_cursor_animation_length = 0.05
  vim.g.neovide_cursor_trail_size = 0.5
  vim.g.neovide_cursor_smooth_blink = true
  vim.g.neovide_cursor_vfx_mode = "railgun"
end

vim.g.lazyvim_picker = "snacks"
vim.g.lazyvim_ruby_lsp = "ruby_lsp"
vim.g.lazyvim_ruby_formatter = "rubocop"

vim.filetype.add({
  extension = {
    rbi = "ruby",
  },
})

vim.opt.relativenumber = true
