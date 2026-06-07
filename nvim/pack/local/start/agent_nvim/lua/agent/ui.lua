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

local NEW_AGENT_LABEL = "✦ new agent (this worktree)"

---Snacks picker over agent items with ANSI viewport preview per session
---(via `_agent_session_dump`). Falls back to `vim.ui.select` if Snacks
---isn't available — no preview, but the picker still works.
---@param opts {title: string, items: {text: string, name?: string, is_new?: boolean}[], on_pick: fun(item?: {text: string, name?: string, is_new?: boolean})}
local function pick_with_preview(opts)
  if Snacks and Snacks.picker then
    Snacks.picker.pick({
      source = "agent_sessions",
      title = opts.title,
      items = opts.items,
      format = function(item) return { { item.text } } end,
      preview = function(ctx)
        if not ctx.item.name then
          ctx.preview:set_title(ctx.item.text)
          ctx.preview:set_lines({})
          return
        end
        ctx.preview:set_title(ctx.item.name)
        return Snacks.picker.preview.cmd(
          { "fish", "-c", "_agent_session_dump " .. vim.fn.shellescape(ctx.item.name) },
          ctx
        )
      end,
      confirm = function(picker, item)
        picker:close()
        vim.schedule(function() opts.on_pick(item) end)
      end,
    })
    return
  end

  ---@type string[]
  local labels = {}
  for _, item in ipairs(opts.items) do table.insert(labels, item.text) end
  vim.ui.select(labels, { prompt = opts.title }, function(_, idx)
    opts.on_pick(idx and opts.items[idx] or nil)
  end)
end

---@param opts {include_new?: boolean, on_pick: fun(name: string|nil, is_new: boolean)}
function M.pick_session(opts)
  local sessions = zellij.list_sessions()
  if #sessions == 0 and not opts.include_new then
    opts.on_pick(nil, false)
    return
  end

  ---@type {text: string, name: string|nil, is_new: boolean}[]
  local items = {}
  if opts.include_new then
    table.insert(items, { text = NEW_AGENT_LABEL, is_new = true })
  end
  for _, s in ipairs(sessions) do
    table.insert(items, { text = s, name = s, is_new = false })
  end

  pick_with_preview({
    title = "agent session",
    items = items,
    on_pick = function(item)
      if not item then return opts.on_pick(nil, false) end
      opts.on_pick(item.name, item.is_new == true)
    end,
  })
end

---@param opts {current?: string, on_pick: fun(name?: string)}
function M.pick_kill(opts)
  local sessions = zellij.list_sessions()
  if #sessions == 0 then opts.on_pick(nil) return end

  ---@type {text: string, name: string}[]
  local items = {}
  if opts.current and vim.tbl_contains(sessions, opts.current) then
    table.insert(items, { text = ("(current) %s"):format(opts.current), name = opts.current })
  end
  for _, s in ipairs(sessions) do
    if s ~= opts.current then table.insert(items, { text = s, name = s }) end
  end

  pick_with_preview({
    title = "kill agent",
    items = items,
    on_pick = function(item) opts.on_pick(item and item.name or nil) end,
  })
end

return M
