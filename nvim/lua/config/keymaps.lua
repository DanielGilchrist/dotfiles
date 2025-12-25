-- Keymaps are automatically loaded on the VeryLazy event
-- Default keymaps that are always set: https://github.com/LazyVim/LazyVim/blob/main/lua/lazyvim/config/keymaps.lua
-- Add any additional keymaps here

local map = vim.keymap.set
local l = function(keys)
  return "<leader>" .. keys
end

-- Navigation
map("n", l("to"), function()
  require("custom.test_open").open_test()
end, { desc = "Switch to and from a corresponding test file" })

-- Debug
map("n", l("wtf"), function()
  local ft = vim.bo.filetype
  if ft == "ruby" or ft == "crystal" then
    vim.cmd('normal! oputs "#" * 90')
    vim.cmd('normal! oputs caller')
    vim.cmd('normal! oputs "#" * 90')
  elseif ft == "haml" then
    vim.cmd('normal! o- puts "#" * 90')
    vim.cmd('normal! o- puts caller')
    vim.cmd('normal! o- puts "#" * 90')
  elseif ft == "eruby" then
    vim.cmd('normal! o<% puts "#" * 90 %>')
    vim.cmd('normal! o<% puts caller %>')
    vim.cmd('normal! o<% puts "#" * 90 %>')
  elseif ft == "rust" then
    vim.cmd('normal! oprintln!("{}", "#".repeat(90));')
    vim.cmd('normal! oeprintln!("{:?}", std::backtrace::Backtrace::capture());')
    vim.cmd('normal! oprintln!("{}", "#".repeat(90));')
  elseif ft == "go" then
    vim.cmd('normal! ofmt.Println(strings.Repeat("#", 90))')
    vim.cmd('normal! odebug.PrintStack()')
    vim.cmd('normal! ofmt.Println(strings.Repeat("#", 90))')
  else
    print("No <leader>wtf definition for '" .. ft .. "'")
  end
end, { desc = "Insert debug trace for current language" })

-- Clipboard
map("n", l("bc"), function()
  local notify = require("../utils/notify")
  local path = vim.fn.expand("%:.")
  vim.fn.setreg('+', path)
  notify.info("Copied " .. path .. " to clipboard")
end, { desc = "Copy relative file path for current buffer" })

-- Music (spotify_player)
local function playback(command)
  local is = require("utils.is")
  local cmd = { "spotify_player", "playback" }

  if is.table(command) then
    vim.list_extend(cmd, command)
  else
    table.insert(cmd, command)
  end

  vim.fn.jobstart(cmd, {
    stderr_buffered = true,
    on_stderr = function(_, data)
      local is = require("utils.is")
      local error_message = table.concat(data)
      if is.empty(error_message) then
        return
      end

      local notify = require("../utils/notify")

      -- "Bad request: no playback found" or "Bad request: no active playback found!"
      if string.find(error_message, "no") and string.find(error_message, "playback") then
        notify.warn("A song isn't currently playing.")
      else
        notify.error(string.format("Unhandled error: \"%s\"", error_message))
      end
    end
  })
end

map("n", l("mo"), function()
  Snacks.terminal.toggle("spotify")
end, { desc = "Open spotify music player" })

map("n", l("mn"), function()
  playback("next")
end, { desc = "Next song" })

map("n", l("mp"), function()
  playback("previous")
end, { desc = "Previous song" })

map("n", l("mt"), function()
  playback("play-pause")
end, { desc = "Toggle play/pause" })

map("n", l("mf"), function()
  playback({ "seek", "10000" })
end, { desc = "Fast-forward 10 seconds" })

map("n", l("mb"), function()
  playback({ "seek", "--", "-10000" })
end, { desc = "Rewind 10 seconds" })

map("n", l("ts"), function()
  Snacks.terminal.toggle("tetrigo")
end, { desc = "Launch Tetris" })

-- btop
map("n", l("bt"), function()
  Snacks.terminal.toggle("btop")
end, { desc = "Launch btop" })
