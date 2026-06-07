-- Bottom-split composer for prompts. The buffer is persistent — toggling
-- the window hides/shows it without losing what you've typed. Submitting
-- (`<C-s>` or `:w`/`:wq`) sends the text to the active agent and clears.

local M = {}

local state = {
  buf = nil, ---@type integer|nil
  win = nil, ---@type integer|nil
}

---@return integer
local function ensure_buf()
  if state.buf and vim.api.nvim_buf_is_valid(state.buf) then return state.buf end
  local buf = vim.api.nvim_create_buf(false, true)
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].buftype = "acwrite" -- allows `:w` to fire BufWriteCmd
  vim.bo[buf].bufhidden = "hide"
  vim.bo[buf].swapfile = false
  pcall(vim.api.nvim_buf_set_name, buf, "agent-composer")
  state.buf = buf

  vim.keymap.set({ "n", "i" }, "<C-s>", M.submit, { buffer = buf, silent = true, desc = "agent: submit" })
  vim.keymap.set("n", "q", M.cancel, { buffer = buf, silent = true, desc = "agent: cancel" })
  vim.keymap.set("n", "<Esc><Esc>", M.cancel, { buffer = buf, silent = true, desc = "agent: cancel" })

  -- `:w` / `:wq` / `:x` fire BufWriteCmd. Submit + clear, but don't close
  -- the window — for `:wq` the `q` half closes it naturally, and for bare
  -- `:w` the user can keep typing in the now-empty composer.
  vim.api.nvim_create_autocmd("BufWriteCmd", {
    buffer = buf,
    callback = function()
      vim.bo[buf].modified = false
      M.submit({ close = false })
    end,
  })

  return buf
end

---@return boolean
function M.is_visible()
  return state.win ~= nil and vim.api.nvim_win_is_valid(state.win)
end

---Close any existing composer window across all tabs.
local function close_all_windows()
  if not state.buf then return end
  for _, win in ipairs(vim.fn.win_findbuf(state.buf)) do
    pcall(vim.api.nvim_win_close, win, true)
  end
  state.win = nil
end

function M.show()
  local buf = ensure_buf()
  if M.is_visible() then
    vim.api.nvim_set_current_win(state.win)
    return
  end
  -- Ensure only one composer window exists (e.g. if user switched tabs
  -- since opening). Then open fresh in the current tab.
  close_all_windows()
  vim.cmd("botright 10split")
  state.win = vim.api.nvim_get_current_win()
  vim.api.nvim_win_set_buf(state.win, buf)
  vim.wo[state.win].winbar = " agent prompt — <C-s>/:wq submit, q/<Esc><Esc> cancel "
  -- Cursor at end so appended content is reachable immediately.
  local last = vim.api.nvim_buf_line_count(buf)
  vim.api.nvim_win_set_cursor(state.win, { last, 0 })
end

function M.hide()
  close_all_windows()
end

function M.toggle()
  if M.is_visible() then M.hide() else M.show() end
end

local function clear()
  if state.buf and vim.api.nvim_buf_is_valid(state.buf) then
    vim.api.nvim_buf_set_lines(state.buf, 0, -1, false, {})
    vim.bo[state.buf].modified = false
  end
end

---@param text string
function M.append(text)
  local buf = ensure_buf()
  local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
  local has_content = false
  for _, l in ipairs(lines) do
    if vim.trim(l) ~= "" then has_content = true; break end
  end
  local new_lines = vim.split(text, "\n", { plain = true })
  if has_content then
    table.insert(lines, "")
    for _, l in ipairs(new_lines) do table.insert(lines, l) end
    table.insert(lines, "")
  else
    lines = new_lines
    table.insert(lines, "")
  end
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
end

---@param opts? {close?: boolean}
function M.submit(opts)
  opts = opts or {}
  local close = opts.close ~= false
  if not state.buf or not vim.api.nvim_buf_is_valid(state.buf) then return end
  local lines = vim.api.nvim_buf_get_lines(state.buf, 0, -1, false)
  local text = vim.trim(table.concat(lines, "\n"))
  if text == "" then
    if close then M.cancel() end
    return
  end
  local sent = require("agent").send_text(text, true)
  if not sent then return end
  clear()
  if close then M.hide() end
end

function M.cancel()
  clear()
  M.hide()
end

return M
