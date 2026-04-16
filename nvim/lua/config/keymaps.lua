local notify = require("utils.notify")
local is = require("utils.is")

local map = vim.keymap.set
local leader = function(keys) return "<leader>" .. keys end

-- Better up/down with wrapped lines
map({ "n", "x" }, "j", "v:count == 0 ? 'gj' : 'j'", { expr = true, silent = true })
map({ "n", "x" }, "k", "v:count == 0 ? 'gk' : 'k'", { expr = true, silent = true })

-- Window navigation
map("n", "<C-h>", "<C-w>h", { desc = "Go to Left Window" })
map("n", "<C-j>", "<C-w>j", { desc = "Go to Lower Window" })
map("n", "<C-k>", "<C-w>k", { desc = "Go to Upper Window" })
map("n", "<C-l>", "<C-w>l", { desc = "Go to Right Window" })

-- Window resize
map("n", "<A-Up>", "<cmd>resize +2<cr>", { desc = "Increase Window Height" })
map("n", "<A-Down>", "<cmd>resize -2<cr>", { desc = "Decrease Window Height" })
map("n", "<A-Left>", "<cmd>vertical resize -2<cr>", { desc = "Decrease Window Width" })
map("n", "<A-Right>", "<cmd>vertical resize +2<cr>", { desc = "Increase Window Width" })

-- Move lines
map("n", "<A-j>", "<cmd>execute 'move .+' . v:count1<cr>==", { desc = "Move Down" })
map("n", "<A-k>", "<cmd>execute 'move .-' . (v:count1 + 1)<cr>==", { desc = "Move Up" })
map("i", "<A-j>", "<esc><cmd>m .+1<cr>==gi", { desc = "Move Down" })
map("i", "<A-k>", "<esc><cmd>m .-2<cr>==gi", { desc = "Move Up" })
map("v", "<A-j>", ":<C-u>execute \"'<,'>move '>+\" . v:count1<cr>gv=gv", { desc = "Move Down" })
map("v", "<A-k>", ":<C-u>execute \"'<,'>move '<-\" . (v:count1 + 1)<cr>gv=gv", { desc = "Move Up" })

-- Buffer navigation
map("n", "<S-h>", "<cmd>bprevious<cr>", { desc = "Prev Buffer" })
map("n", "<S-l>", "<cmd>bnext<cr>", { desc = "Next Buffer" })
map("n", "[b", "<cmd>bprevious<cr>", { desc = "Prev Buffer" })
map("n", "]b", "<cmd>bnext<cr>", { desc = "Next Buffer" })

map("n", leader("bd"), function() Snacks.bufdelete() end, { desc = "Delete Buffer" })
map("n", leader("bD"), function() Snacks.bufdelete.all() end, { desc = "Delete All Buffers" })
map("n", leader("bo"), function() Snacks.bufdelete.other() end, { desc = "Delete Other Buffers" })

-- Clear search highlight
map("n", "<Esc>", "<cmd>nohlsearch<cr><Esc>", { desc = "Clear Search Highlight" })

-- Better indenting (stay in visual mode)
map("v", "<", "<gv")
map("v", ">", ">gv")

-- Quit
map("n", leader("qq"), "<cmd>qa<cr>", { desc = "Quit All" })

-- Windows
map("n", leader("-"), "<C-W>s", { desc = "Split Below" })
map("n", leader("|"), "<C-W>v", { desc = "Split Right" })

-- Terminal
map("t", "<Esc><Esc>", "<C-\\><C-n>", { desc = "Exit Terminal Mode" })

-- Navigation
map("n", leader("to"), "<cmd>TestOpen<cr>", { desc = "Switch to/from test file" })

-- Debug trace
map("n", leader("wtf"), function()
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
  elseif ft == "lua" then
    vim.cmd('normal! oprint(string.rep("#", 90))')
    vim.cmd('normal! oprint(debug.traceback())')
    vim.cmd('normal! oprint(string.rep("#", 90))')
  else
    print("No <leader>wtf definition for '" .. ft .. "'")
  end
end, { desc = "fuck?" })

-- Copy file path
map("n", leader("bc"), function()
  local path = vim.fn.expand("%:.")
  vim.fn.setreg("+", path)
  notify.info("Copied " .. path .. " to clipboard")
end, { desc = "Copy relative file path" })

-- Spotify
local function playback(command)
  local cmd = { "spotify_player", "playback" }
  if is.table(command) then
    vim.list_extend(cmd, command)
  else
    table.insert(cmd, command)
  end
  vim.fn.jobstart(cmd, {
    stderr_buffered = true,
    on_stderr = function(_, data)
      local error_message = table.concat(data)
      if is.empty(error_message) then return end
      if string.find(error_message, "no") and string.find(error_message, "playback") then
        notify.warn("A song isn't currently playing.")
      else
        notify.error(string.format("Unhandled error: \"%s\"", error_message))
      end
    end,
  })
end

map("n", leader("mo"), function() Snacks.terminal.toggle("spotify") end, { desc = "Open spotify" })
map("n", leader("mn"), function() playback("next") end, { desc = "Next song" })
map("n", leader("mp"), function() playback("previous") end, { desc = "Previous song" })
map("n", leader("mt"), function() playback("play-pause") end, { desc = "Toggle play/pause" })
map("n", leader("mf"), function() playback({ "seek", "10000" }) end, { desc = "Fast-forward 10s" })
map("n", leader("mb"), function() playback({ "seek", "--", "-10000" }) end, { desc = "Rewind 10s" })

local terminal_state = { last = nil }

map("t", "<C-;>", function()
  local buf = vim.api.nvim_get_current_buf()
  for _, terminal in pairs(Snacks.terminal.list()) do
    if terminal.buf == buf then
      terminal_state.last = terminal
      terminal:hide()
      return
    end
  end
end, { desc = "Toggle Terminal" })

map("n", "<C-;>", function()
  local terminal = terminal_state.last
  if terminal then
    terminal:show()
  end
end, { desc = "Toggle Terminal" })

-- Lazygit
map("n", leader("gg"), function() Snacks.lazygit({ cwd = require("utils.path").root() }) end, { desc = "Lazygit" })

-- btop / tetris
map("n", leader("ts"), function() Snacks.terminal.toggle("tetrigo") end, { desc = "Launch Tetris" })
map("n", leader("bt"), function() Snacks.terminal.toggle("btop") end, { desc = "Launch btop" })
