-- Keymaps are automatically loaded on the VeryLazy event
-- Default keymaps that are always set: https://github.com/LazyVim/LazyVim/blob/main/lua/lazyvim/config/keymaps.lua
-- Add any additional keymaps here

local map = vim.keymap.set
local l = function(keys)
  return "<leader>" .. keys
end

-- Gitlinker keymaps
map("n", l("gy"), function()
  local url = require("gitlinker").get_buf_range_url("n")
  vim.fn.setreg("+", url)
end, { desc = "Copy remote link to clipboard", noremap = true })

map("v", l("gy"), function()
  local url = require("gitlinker").get_buf_range_url("v")
  vim.fn.setreg("+", url)
end, { desc = "Copy remote link to clipboard", noremap = true })

-- Diagnostic keymaps
map("n", "]d", vim.diagnostic.goto_next, { desc = "Go to next diagnostic" })

map("n", l("cS"), function()
  require("treesj").toggle({ split = { recursive = true } })
end, { desc = "treesj toggle" })

-- custom keymaps
map("n", l("jC"), function()
  local notify = require("../utils/notify")
  local clipboard_contents = vim.fn.getreg("+")
  local file_path, line = clipboard_contents:match("(.+):(%d+)")

  if not file_path then
    file_path = clipboard_contents:match("(.+)$")
  end

  if not file_path or not vim.loop.fs_stat(file_path) then
    local message = string.format("\"%s\" is not a valid file to jump to!", file_path)
    return notify.error(message)
  end

  local command = "edit " .. file_path

  if line then
    command = command .. " | " .. line
  end

  vim.cmd(command)
end, { desc = "Jump to file from clipboard" })

map("n", l("mo"), function()
  Snacks.terminal.toggle("spotify_player", {
    win = {
      style = "terminal",
      keys = {
        ["<esc>"] = "hide",
        ["<C-/>"] = "hide",
      }
    }
  })
end, { desc = "Open spotify music player" })
