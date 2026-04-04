local autocmd = vim.api.nvim_create_autocmd

-- Check if file changed outside Neovim
autocmd({ "FocusGained", "TermClose", "TermLeave" }, {
  callback = function()
    if vim.o.buftype ~= "nofile" then vim.cmd("checktime") end
  end,
})

-- Highlight on yank
autocmd("TextYankPost", {
  callback = function()
    vim.hl.on_yank()
  end,
})

-- Resize splits on window resize
autocmd("VimResized", {
  callback = function()
    local current_tab = vim.fn.tabpagenr()
    vim.cmd("tabdo wincmd =")
    vim.cmd("tabnext " .. current_tab)
  end,
})

-- Go to last cursor position when opening a buffer
autocmd("BufReadPost", {
  callback = function(ev)
    local exclude = { "gitcommit" }
    local buf = ev.buf
    if vim.tbl_contains(exclude, vim.bo[buf].filetype) or vim.b[buf]._last_loc then
      return
    end
    vim.b[buf]._last_loc = true
    local mark = vim.api.nvim_buf_get_mark(buf, '"')
    local lcount = vim.api.nvim_buf_line_count(buf)
    if mark[1] > 0 and mark[1] <= lcount then
      pcall(vim.api.nvim_win_set_cursor, 0, mark)
    end
  end,
})

-- Close certain filetypes with q
autocmd("FileType", {
  pattern = { "help", "notify", "checkhealth", "qf", "man", "grug-far" },
  callback = function(ev)
    vim.bo[ev.buf].buflisted = false
    vim.keymap.set("n", "q", "<cmd>close<cr>", { buf = ev.buf, silent = true })
  end,
})

-- Strip trailing whitespace on save
autocmd("BufWritePre", {
  pattern = "*",
  command = [[%s/\s\+$//e]],
})

-- Relative number toggle in insert mode
autocmd("InsertEnter", {
  callback = function() vim.opt.relativenumber = false end,
})
autocmd("InsertLeave", {
  callback = function() vim.opt.relativenumber = true end,
})

-- Line wrap for specific filetypes
autocmd("FileType", {
  pattern = { "markdown", "haml" },
  callback = function()
    vim.opt_local.wrap = true
    vim.opt_local.linebreak = true
  end,
})

-- Suppress "[Process exited 0]" virtual text in terminal buffers (0.12 feature)
local term_acs = vim.api.nvim_get_autocmds({ group = "nvim.terminal", event = "TermClose" })
for _, ac in ipairs(term_acs) do
  if ac.desc and ac.desc:match("Process exited") then
    vim.api.nvim_del_autocmd(ac.id)
  end
end

-- Crystal treesitter registration
vim.treesitter.language.register("crystal", { "cr" })

-- Dashboard command
vim.api.nvim_create_user_command("Dashboard", function()
  Snacks.dashboard()
end, {})
