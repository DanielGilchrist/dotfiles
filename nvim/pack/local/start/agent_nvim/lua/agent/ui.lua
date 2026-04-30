local zellij = require("agent.zellij")

---@class AgentUI
---@field new_prompt fun(on_submit: fun(text: string)): nil
---@field pick_session fun(opts: {include_new?: boolean, on_pick: fun(name: string|nil, is_new: boolean)}): nil
local M = {}

---@param on_submit fun(text: string)
function M.new_prompt(on_submit)
  local buf = vim.api.nvim_create_buf(false, true)
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"

  local has_snacks, Snacks = pcall(require, "snacks")
  ---@type integer|nil
  local win_id

  local function close()
    if win_id and vim.api.nvim_win_is_valid(win_id) then
      vim.api.nvim_win_close(win_id, true)
    end
    win_id = nil
  end

  local function submit()
    local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
    local text = vim.trim(table.concat(lines, "\n"))
    close()
    if text ~= "" then on_submit(text) end
  end

  if has_snacks and Snacks and Snacks.win then
    local snacks_win = Snacks.win({
      buf = buf,
      title = " new agent prompt — <C-s> submit, q/<C-c> cancel ",
      width = 0.7,
      height = 0.5,
      border = "rounded",
      keys = {
        q = function() close() end,
      },
      bo = { filetype = "markdown" },
      wo = { wrap = true, linebreak = true, breakindent = true },
    })
    win_id = snacks_win.win
  else
    local width = math.floor(vim.o.columns * 0.7)
    local height = math.floor(vim.o.lines * 0.5)
    win_id = vim.api.nvim_open_win(buf, true, {
      relative = "editor",
      width = width,
      height = height,
      row = math.floor((vim.o.lines - height) / 2),
      col = math.floor((vim.o.columns - width) / 2),
      style = "minimal",
      border = "rounded",
      title = " new agent prompt — <C-s> submit, q/<C-c> cancel ",
    })
  end

  local map_opts = { buffer = buf, nowait = true, silent = true }
  vim.keymap.set({ "n", "i" }, "<C-s>", submit, map_opts)
  vim.keymap.set("n", "q", close, map_opts)
  vim.keymap.set("i", "<C-c>", close, map_opts)
  vim.cmd("startinsert")
end

---@param opts {include_new?: boolean, on_pick: fun(name: string|nil, is_new: boolean)}
function M.pick_session(opts)
  local sessions = zellij.list_sessions()
  ---@type string[]
  local items = {}
  if opts.include_new then table.insert(items, "✦ new agent (this worktree)") end
  for _, s in ipairs(sessions) do table.insert(items, s) end

  vim.ui.select(items, { prompt = "agent session" }, function(choice, idx)
    if not choice or not idx then return opts.on_pick(nil, false) end
    if opts.include_new and idx == 1 then
      return opts.on_pick(nil, true)
    end
    opts.on_pick(choice, false)
  end)
end

return M
