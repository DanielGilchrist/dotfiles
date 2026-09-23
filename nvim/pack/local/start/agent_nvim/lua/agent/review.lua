local config = require("agent.config")
local ui = require("agent.ui")

local M = {}

local ns = vim.api.nvim_create_namespace("agent_review")
local edit_ns = vim.api.nvim_create_namespace("agent_review_editing")

---@class ReviewComment
---@field id integer
---@field bufnr integer|nil
---@field extmark integer|nil
---@field file string
---@field relpath string
---@field ft string
---@field lnum integer
---@field nlines integer
---@field code string[]
---@field text string
---@field file_level boolean|nil

---@type ReviewComment[]
M.comments = {}
M._next_id = 1
M._base = nil ---@type string|nil
M._total = 0
M._active = false
M._root = nil ---@type string|nil
M._message = nil ---@type string|nil
---@type table<string, boolean>
M.reviewed = {}

local cmd_utils = require("utils.cmd")
local file_utils = require("utils.file")
local is = require("utils.is")
local path_utils = require("utils.path")
local ui_utils = require("utils.ui")
local notify = require("utils.notify").with_title("agent review")

local function rcfg() return config.review or {} end

---@return boolean
local function has_message()
  return M._message ~= nil and is.not_empty(vim.trim(M._message))
end

---@param text string
---@param width integer
---@return string[]
local function wrap_text(text, width)
  width = math.max(width, 20)
  local out = {}
  for _, para in ipairs(vim.split(text, "\n", { plain = true })) do
    if is.empty(para) then
      out[#out + 1] = ""
    else
      local line = ""
      for word in para:gmatch("%S+") do
        if is.empty(line) then
          line = word
        elseif #line + 1 + #word <= width then
          line = line .. " " .. word
        else
          out[#out + 1] = line
          line = word
        end
      end
      if is.not_empty(line) then out[#out + 1] = line end
    end
  end
  return out
end

---@param dir string
---@param args string[]
---@return string|nil
local function git(dir, args)
  local cmd = { "git", "-C", dir }
  vim.list_extend(cmd, args)
  local res = vim.system(cmd, { text = true }):wait()
  if not cmd_utils.success(res.code) then return nil end
  return vim.trim(res.stdout or "")
end

---@param path string|nil
---@return string|nil
local function repo_root(path)
  local dir = is.not_empty(path) and vim.fn.fnamemodify(path, ":h") or vim.fn.getcwd()
  if is.not_directory(dir) then dir = vim.fn.getcwd() end
  return git(dir, { "rev-parse", "--show-toplevel" })
end

---@param root string
---@return string[] remotes, "origin" first
local function remotes(root)
  local out = git(root, { "remote" })
  local names = out and vim.split(out, "\n", { trimempty = true }) or {}
  table.sort(names, function(a, b)
    if a == "origin" or b == "origin" then return a == "origin" end
    return a < b
  end)
  return names
end

---@param root string
---@return string base, string label
local function fork_point(root)
  local candidates = {}
  for _, remote in ipairs(remotes(root)) do
    vim.list_extend(candidates, { remote .. "/HEAD", remote .. "/main", remote .. "/master" })
  end
  -- @{upstream} last: for a pushed branch it's the branch's own remote-tracking
  -- ref, whose merge-base is just HEAD, so it must not shadow real candidates.
  vim.list_extend(candidates, { "main", "master", "@{upstream}" })
  for _, ref in ipairs(candidates) do
    if git(root, { "rev-parse", "--verify", "--quiet", ref }) then
      local mb = git(root, { "merge-base", "HEAD", ref })
      if is.not_empty(mb) then
        local label = ref:match("/HEAD$") and git(root, { "rev-parse", "--abbrev-ref", ref }) or ref
        return mb, label or ref
      end
    end
  end
  return "HEAD", "HEAD"
end

---@param root string
---@return boolean
local function is_trunk(root)
  local branch = git(root, { "rev-parse", "--abbrev-ref", "HEAD" })
  if not branch or branch == "HEAD" then return false end
  if branch == "main" or branch == "master" then return true end
  for _, remote in ipairs(remotes(root)) do
    local def = git(root, { "rev-parse", "--abbrev-ref", remote .. "/HEAD" })
    if def and branch == def:sub(#remote + 2) then return true end
  end
  return false
end

---@param root string
---@return string base, string label
local function resolve_base(root)
  if rcfg().base == "head" then return "HEAD", "HEAD" end
  return fork_point(root)
end

---@return string|nil
local function current_base()
  return M._base
end

---@param root string
---@param base string
---@return {text: string, file: string}[]
local function list_changed(root, base)
  local tracked = git(root, { "diff", "--name-only", base }) or ""
  local untracked = git(root, { "ls-files", "--others", "--exclude-standard" }) or ""
  local numstat = git(root, { "diff", "--numstat", "--no-renames", base }) or ""
  local stat = {}
  for line in numstat:gmatch("[^\n]+") do
    local a, d, rel = line:match("^(%S+)\t(%S+)\t(.+)$")
    if rel then stat[vim.trim(rel)] = { add = tonumber(a) or 0, del = tonumber(d) or 0 } end
  end
  local seen, items = {}, {}
  local function add(rel, is_untracked)
    rel = vim.trim(rel)
    if is.empty(rel) or seen[rel] then return end
    seen[rel] = true
    local s = stat[rel]
    local adds, dels = 0, 0
    if s then
      adds, dels = s.add, s.del
    elseif is_untracked then
      local ok, lines = pcall(vim.fn.readfile, root .. "/" .. rel)
      if ok then adds = #lines end
    end
    items[#items + 1] = { text = rel, file = root .. "/" .. rel, untracked = is_untracked, add = adds, del = dels }
  end
  for line in tracked:gmatch("[^\n]+") do add(line, false) end
  for line in untracked:gmatch("[^\n]+") do add(line, true) end
  return items
end

---@param root string
---@return string
local function state_path(root)
  return vim.fn.stdpath("data") .. "/agent_review/" .. (root:gsub("[/\\]", "%%")) .. ".json"
end

---@param root string
---@return table|nil
local function read_state(root)
  local raw = file_utils.read(state_path(root))
  if not raw then return nil end
  local ok, data = pcall(vim.json.decode, raw)
  if ok and is.table(data) then return data end
  return nil
end

---@param root string
local function clear_state(root)
  pcall(vim.fn.delete, state_path(root))
end

---@param buf integer
---@param s integer
---@param e integer
local function highlight_range(buf, s, e)
  local last = vim.api.nvim_buf_line_count(buf) - 1
  for l = s, e do
    local row = math.min(l - 1, last)
    pcall(vim.api.nvim_buf_set_extmark, buf, edit_ns, row, 0, {
      line_hl_group = rcfg().editing_hl or "Visual",
      priority = 20000,
    })
  end
end

---@param buf integer
local function clear_highlight(buf)
  pcall(vim.api.nvim_buf_clear_namespace, buf, edit_ns, 0, -1)
end

M._diffed = {}

---@param buf integer
local function ensure_diff(buf)
  if not M._active then return end
  if not (buf and vim.api.nvim_buf_is_valid(buf)) then return end
  if M._diffed[buf] then return end
  if is.not_empty(vim.bo[buf].buftype) then return end
  local file = vim.api.nvim_buf_get_name(buf)
  if is.empty(file) then return end
  if M._root and not vim.startswith(file, M._root .. "/") then return end
  M._diffed[buf] = true
  pcall(function()
    require("unified.diff").show(current_base() or "HEAD", buf)
    require("unified.auto_refresh").setup(buf)
  end)
end

---@param file string
---@param base string|nil
---@param pos? integer[]
local function open_in_diff(file, base, pos)
  vim.cmd("edit " .. vim.fn.fnameescape(file))
  ensure_diff(vim.api.nvim_get_current_buf())
  if pos then pcall(vim.api.nvim_win_set_cursor, 0, pos) end
end

---@return boolean
function M.is_active()
  return M._active
end

---@return boolean
local function require_active()
  if M.is_active() then return true end
  notify.warn("no active review; run <leader>arr first")
  return false
end

---@return string
function M.status()
  if not M.is_active() then return "" end
  local done = 0
  for _ in pairs(M.reviewed) do done = done + 1 end
  local s = ("review %d/%d"):format(done, M._total or 0)
  if #M.comments > 0 then s = s .. (" · %d✎"):format(#M.comments) end
  return s
end

function M.next_hunk()
  if M.is_active() then require("unified.navigation").next_hunk() end
end

function M.prev_hunk()
  if M.is_active() then require("unified.navigation").previous_hunk() end
end

---@param root string
---@param base string
---@param label string
local function open_review(root, base, label)
  M._active = true
  M._root = root
  M._base = base
  M._diffed = {}
  M._total = #list_changed(root, base)
  vim.cmd("Unified " .. base)
  M._diffed[vim.api.nvim_get_current_buf()] = true
  notify.info(("reviewing vs %s (%s)"):format(label, base:sub(1, 8)))
end

---@param root string
---@param cb fun(sha: string)
function M.pick_commit(root, cb)
  local log = git(root, { "log", "--oneline", "-n", "100", "--no-decorate" }) or ""
  ---@type {text: string, sha: string}[]
  local items = {}
  for line in log:gmatch("[^\n]+") do
    local sha = line:match("^(%S+)")
    if sha then items[#items + 1] = { text = line, sha = sha } end
  end
  if #items == 0 then return notify.info("no commits to pick") end

  Snacks.picker.pick({
    source = "agent_review_commits",
    title = "review since commit",
    items = items,
    format = function(item) return { { item.text } } end,
    preview = function(ctx)
      return Snacks.picker.preview.cmd({ "git", "-C", root, "show", "--stat", ctx.item.sha }, ctx, { ft = "git" })
    end,
    confirm = function(picker, item)
      picker:close()
      if item then vim.schedule(function() cb(item.sha) end) end
    end,
  })
end

---@param root string
function M.pick_mode(root)
  local head = git(root, { "rev-parse", "HEAD" })
  local fork, fork_label = fork_point(root)
  local has_branch = not is_trunk(root) and fork ~= "HEAD" and fork ~= head

  local working = {
    text = "Working tree (uncommitted changes)",
    run = function() open_review(root, "HEAD", "working tree") end,
  }
  local branch = has_branch and {
    text = ("Branch (all commits since %s)"):format(fork_label),
    run = function() open_review(root, fork, fork_label) end,
  } or nil
  local commit = {
    text = "Since a commit…",
    run = function() M.pick_commit(root, function(sha) open_review(root, sha, sha:sub(1, 8)) end) end,
  }

  ---@type {text: string, run: fun()}[]
  local options
  if branch and rcfg().base ~= "head" then
    options = { branch, working, commit }
  else
    options = { working }
    if branch then table.insert(options, branch) end
    table.insert(options, commit)
  end

  vim.ui.select(options, {
    prompt = "Review mode:",
    format_item = function(item) return item.text end,
  }, function(choice)
    if choice then choice.run() end
  end)
end

function M.start()
  if M.is_active() then
    if #M.comments > 0 then
      ui_utils.confirm(("Exit review? %d pending comment(s) will be discarded."):format(#M.comments), function(choice)
        if choice == "Ok" then M.reset() end
      end)
    else
      M.reset()
    end
    return
  end

  local root = repo_root(vim.api.nvim_buf_get_name(0))
  if not root then return notify.warn("not in a git repo") end

  local saved = read_state(root)
  if saved then
    vim.ui.select({ "resume", "start fresh" }, {
      prompt = ("Resume review? %d comment(s), %d file(s) marked"):format(#(saved.comments or {}), #(saved.reviewed or {})),
    }, function(choice)
      if choice == "resume" then
        M.resume(root, saved)
      elseif choice == "start fresh" then
        clear_state(root)
        M.pick_mode(root)
      end
    end)
    return
  end

  M.pick_mode(root)
end

---@param root string|nil
---@param base string|nil
function M.changed_files(root, base)
  if not require_active() then return end
  root = root or repo_root(vim.api.nvim_buf_get_name(0))
  if not root then return notify.warn("not in a git repo") end
  base = base or current_base() or select(1, resolve_base(root))

  local items = list_changed(root, base)
  M._total = #items
  if #items == 0 then return notify.info("no changes vs " .. base:sub(1, 8)) end

  local ordered, tadd, tdel = {}, 0, 0
  for _, it in ipairs(items) do
    tadd, tdel = tadd + (it.add or 0), tdel + (it.del or 0)
  end
  for _, it in ipairs(items) do if not M.reviewed[it.file] then ordered[#ordered + 1] = it end end
  for _, it in ipairs(items) do if M.reviewed[it.file] then ordered[#ordered + 1] = it end end

  Snacks.picker.pick({
    source = "agent_review_files",
    title = ("changed vs %s   +%d -%d"):format(base:sub(1, 8), tadd, tdel),
    items = ordered,
    format = function(item)
      local segs = M.reviewed[item.file]
        and { { "✓ ", "DiagnosticOk" }, { item.text, "Comment" } }
        or { { "  " }, { item.text, "SnacksPickerFile" } }
      segs[#segs + 1] = { "  +" .. (item.add or 0), "Added" }
      segs[#segs + 1] = { " -" .. (item.del or 0), "Removed" }
      return segs
    end,
    preview = function(ctx)
      local cmd
      if ctx.item.untracked then
        cmd = { "sh", "-c", ("git -C %s diff --no-index -- /dev/null %s || true"):format(
          vim.fn.shellescape(root), vim.fn.shellescape(ctx.item.text)) }
      else
        cmd = { "git", "-C", root, "diff", base, "--", ctx.item.text }
      end
      return Snacks.picker.preview.cmd(cmd, ctx, { ft = "diff" })
    end,
    confirm = function(picker, item)
      picker:close()
      if item then vim.schedule(function() open_in_diff(item.file, base) end) end
    end,
    actions = {
      agent_toggle_reviewed = function(picker, item)
        if not item then return end
        local now = not M.reviewed[item.file]
        M.reviewed[item.file] = now or nil
        picker:refresh()
        if now then picker.list:move(1) end
      end,
    },
    win = {
      input = { keys = { ["<a-m>"] = { "agent_toggle_reviewed", mode = { "i", "n" } } } },
      list = { keys = { ["m"] = "agent_toggle_reviewed" } },
    },
  })
end

function M.toggle_reviewed()
  if not require_active() then return end
  local file = vim.api.nvim_buf_get_name(0)
  if is.empty(file) or is.not_empty(vim.bo.buftype) then
    return notify.warn("not a reviewable file")
  end
  M.reviewed[file] = (not M.reviewed[file]) or nil

  local root = repo_root(file)
  local total = (root and #list_changed(root, current_base() or "HEAD")) or 0
  M._total = total
  local done = 0
  for _ in pairs(M.reviewed) do done = done + 1 end
  local rel = (root and vim.startswith(file, root .. "/")) and file:sub(#root + 2)
    or path_utils.basename(file)
  notify.info(("%s %s (%d/%d reviewed)"):format(M.reviewed[file] and "✓ marked" or "○ unmarked", rel, done, total))
end

---@return integer s, integer e
local function target_range()
  local mode = vim.fn.mode()
  if mode:match("^[vV\022]") then
    local s, e = vim.fn.line("v"), vim.fn.line(".")
    if s > e then s, e = e, s end
    vim.api.nvim_feedkeys(vim.api.nvim_replace_termcodes("<Esc>", true, false, true), "nx", false)
    return s, e
  end
  local l = vim.fn.line(".")
  return l, l
end

---@param c ReviewComment
function M._render(c)
  if not (c.bufnr and vim.api.nvim_buf_is_valid(c.bufnr)) then return end
  if c.extmark then pcall(vim.api.nvim_buf_del_extmark, c.bufnr, ns, c.extmark) end

  local cfg = rcfg()
  local prefix = "  " .. (cfg.icon or "▌") .. " "
  local indent = "    "

  local win = vim.fn.bufwinid(c.bufnr)
  local width = vim.o.columns
  if win ~= -1 then
    local info = vim.fn.getwininfo(win)[1]
    if info then width = info.width - (info.textoff or 0) end
  end
  local avail = math.max(width - #indent - 2, 20)

  local body = c.file_level and ("(file) " .. c.text) or c.text
  ---@type table[]
  local virt = {}
  for i, line in ipairs(wrap_text(body, avail)) do
    virt[#virt + 1] = { { (i == 1 and prefix or indent) .. line, cfg.virt_hl or "Comment" } }
  end

  if c.file_level then
    c.extmark = vim.api.nvim_buf_set_extmark(c.bufnr, ns, 0, 0, {
      sign_text = cfg.sign or "▌",
      sign_hl_group = cfg.sign_hl or "DiagnosticInfo",
      virt_lines = virt,
      virt_lines_above = true,
    })
    return
  end

  local last = vim.api.nvim_buf_line_count(c.bufnr) - 1
  local row = math.min(c.lnum - 1, last)
  c.extmark = vim.api.nvim_buf_set_extmark(c.bufnr, ns, row, 0, {
    sign_text = cfg.sign or "▌",
    sign_hl_group = cfg.sign_hl or "DiagnosticInfo",
    virt_lines = virt,
    virt_lines_above = false,
    end_row = math.min(c.lnum - 1 + (c.nlines - 1), last),
    hl_group = cfg.range_hl,
  })
end

---@param c ReviewComment
---@return integer
local function current_lnum(c)
  if c.bufnr and c.extmark and vim.api.nvim_buf_is_valid(c.bufnr) then
    local pos = vim.api.nvim_buf_get_extmark_by_id(c.bufnr, ns, c.extmark, {})
    if pos and pos[1] then return pos[1] + 1 end
  end
  return c.lnum
end

---@param opts? {range?: integer[], file_level?: boolean}
function M.add_comment(opts)
  if not require_active() then return end
  opts = opts or {}
  local buf = vim.api.nvim_get_current_buf()
  if is.not_empty(vim.bo[buf].buftype) then
    return notify.warn("can only comment on file buffers")
  end
  local file = vim.api.nvim_buf_get_name(buf)
  if is.empty(file) then return notify.warn("buffer has no file path") end

  local root = repo_root(file)
  local relpath = vim.fn.fnamemodify(file, ":.")
  if root and vim.startswith(file, root .. "/") then relpath = file:sub(#root + 2) end
  local ft = vim.bo[buf].filetype
  local file_level = opts.file_level or false

  local s, e, code, loc
  if file_level then
    s, e, code = 1, 1, {}
    loc = relpath .. " (whole file)"
  else
    if opts.range then
      s, e = opts.range[1], opts.range[2]
      if s > e then s, e = e, s end
    else
      s, e = target_range()
    end
    if vim.bo[buf].modified then pcall(vim.cmd, "silent! update") end
    code = vim.api.nvim_buf_get_lines(buf, s - 1, e, false)
    loc = e > s and ("%s:%d-%d"):format(relpath, s, e) or ("%s:%d"):format(relpath, s)
    highlight_range(buf, s, e)
  end

  ui.new_prompt(function(text)
    text = vim.trim(text)
    if is.empty(text) then return end
    ---@type ReviewComment
    local c = {
      id = M._next_id,
      bufnr = buf,
      file = file,
      relpath = relpath,
      ft = ft,
      lnum = s,
      nlines = e - s + 1,
      code = code,
      text = text,
      file_level = file_level or nil,
    }
    M._next_id = M._next_id + 1
    M.comments[#M.comments + 1] = c
    M._render(c)
    notify.info(("comment added (%d pending)"):format(#M.comments))
  end, {
    title = (" review: %s   (<C-s> submit, q cancel) "):format(loc),
    split = true,
    on_close = function() if not file_level then clear_highlight(buf) end end,
  })
end

function M.add_file_comment()
  M.add_comment({ file_level = true })
end

function M.set_message()
  if not require_active() then return end
  ui.new_prompt(function(text)
    M._message = vim.trim(text)
    notify.info(is.not_empty(M._message) and "review message set" or "review message cleared")
  end, {
    title = " review message   (<C-s> save, q cancel) ",
    initial = M._message or "",
    split = true,
  })
end

---@param c ReviewComment
local function forget(c)
  if c.bufnr and c.extmark and vim.api.nvim_buf_is_valid(c.bufnr) then
    pcall(vim.api.nvim_buf_del_extmark, c.bufnr, ns, c.extmark)
  end
end

function M.remove_at_cursor()
  if not require_active() then return end
  local buf = vim.api.nvim_get_current_buf()
  local line = vim.fn.line(".")
  for i, c in ipairs(M.comments) do
    if c.bufnr == buf then
      local s = current_lnum(c)
      if line >= s and line <= s + c.nlines - 1 then
        forget(c)
        table.remove(M.comments, i)
        return notify.info(("comment removed (%d pending)"):format(#M.comments))
      end
    end
  end
  notify.info("no comment under cursor")
end

function M.edit_comment()
  if not require_active() then return end
  local buf = vim.api.nvim_get_current_buf()
  local line = vim.fn.line(".")
  for _, c in ipairs(M.comments) do
    if c.bufnr == buf then
      local s = current_lnum(c)
      if line >= s and line <= s + c.nlines - 1 then
        highlight_range(buf, s, s + c.nlines - 1)
        ui.new_prompt(function(text)
          text = vim.trim(text)
          if is.empty(text) then return end
          c.text = text
          M._render(c)
          notify.info("comment updated")
        end, {
          title = (" edit comment: %s   (<C-s> save, q cancel) "):format(c.relpath),
          initial = c.text,
          split = true,
          on_close = function() clear_highlight(buf) end,
        })
        return
      end
    end
  end
  notify.info("no comment under cursor")
end

function M.clear()
  for _, c in ipairs(M.comments) do forget(c) end
  M.comments = {}
end

function M.list()
  if not require_active() then return end
  if #M.comments == 0 then return notify.info("no pending comments") end

  ---@type table[]
  local items = {}
  for _, c in ipairs(M.comments) do
    local ln = current_lnum(c)
    local first = vim.split(c.text, "\n", { plain = true })[1]
    local label = c.file_level
      and ("%s (file)  %s"):format(c.relpath, first)
      or ("%s:%d  %s"):format(c.relpath, ln, first)
    items[#items + 1] = {
      text = label,
      file = c.file,
      pos = { ln, 0 },
      comment = c,
    }
  end

  Snacks.picker.pick({
    source = "agent_review_comments",
    title = ("review comments (%d)"):format(#M.comments),
    items = items,
    format = function(item) return { { item.text } } end,
    preview = function(ctx)
      local c = ctx.item.comment
      local lines = c.file_level
        and { "# " .. c.relpath .. " (whole file)", "" }
        or { "# " .. c.relpath .. ":" .. current_lnum(c), "" }
      if not c.file_level then
        vim.list_extend(lines, c.code)
        lines[#lines + 1] = ""
      end
      lines[#lines + 1] = "── comment ──"
      vim.list_extend(lines, vim.split(c.text, "\n", { plain = true }))
      ctx.preview:set_lines(lines)
      ctx.preview:set_title(c.relpath)
    end,
    confirm = function(picker, item)
      picker:close()
      if item then vim.schedule(function() open_in_diff(item.file, current_base(), item.pos) end) end
    end,
  })
end

---@return string
local function build_markdown()
  local out = { "# Review comments", "" }
  if has_message() then
    out[#out + 1] = "## Overview"
    out[#out + 1] = ""
    vim.list_extend(out, vim.split(M._message, "\n", { plain = true }))
    out[#out + 1] = ""
  end
  out[#out + 1] = "Notes on your recent changes. Please work through each one."
  out[#out + 1] = ""
  for i, c in ipairs(M.comments) do
    if c.file_level then
      out[#out + 1] = ("## %d. %s (whole file)"):format(i, c.relpath)
      out[#out + 1] = ""
      vim.list_extend(out, vim.split(c.text, "\n", { plain = true }))
      out[#out + 1] = ""
    else
      local ln = current_lnum(c)
      local loc = c.nlines > 1
        and ("%s:%d-%d"):format(c.relpath, ln, ln + c.nlines - 1)
        or ("%s:%d"):format(c.relpath, ln)
      out[#out + 1] = ("## %d. %s"):format(i, loc)
      out[#out + 1] = ""
      out[#out + 1] = "```" .. (c.ft or "")
      vim.list_extend(out, c.code)
      out[#out + 1] = "```"
      out[#out + 1] = ""
      vim.list_extend(out, vim.split(c.text, "\n", { plain = true }))
      out[#out + 1] = ""
    end
  end
  return table.concat(out, "\n")
end

---@param root string
---@param entry string
local function add_local_exclude(root, entry)
  local rel = git(root, { "rev-parse", "--git-path", "info/exclude" })
  if not rel then return end
  local path = vim.startswith(rel, "/") and rel or (root .. "/" .. rel)
  for line in (file_utils.read(path) or ""):gmatch("[^\n]*") do
    if vim.trim(line) == entry then return end
  end
  file_utils.append(path, "\n" .. entry .. "\n")
end

---@param md string
---@param count integer
local function deliver(md, count)
  local agent = require("agent")
  local cfg = rcfg()

  if cfg.delivery == "inline" then
    if agent.send_text(md, true) then
      notify.info(("sent %d comment(s) inline"):format(count))
      M.reset()
    end
    return
  end

  local root = repo_root(vim.api.nvim_buf_get_name(0)) or vim.fn.getcwd()
  local fname = cfg.review_file or ".agent-review.md"
  local path = root .. "/" .. fname
  if not file_utils.write(path, md) then return notify.error("could not write " .. path) end
  add_local_exclude(root, fname)

  local msg = ("Read `%s` in the repo root. It lists my review comments on your recent changes. Work through each one, then delete the file."):format(fname)
  if agent.send_text(msg, true) then
    notify.info(("sent %d comment(s) → %s"):format(count, fname))
    M.reset()
  end
end

---@param count integer
local function preview_send(count)
  ---@type string[][]
  local pages = {}
  if has_message() then
    local lines = { "# Overview message", "" }
    vim.list_extend(lines, vim.split(M._message, "\n", { plain = true }))
    pages[#pages + 1] = lines
  end
  for _, c in ipairs(M.comments) do
    local lines
    if c.file_level then
      lines = { "## " .. c.relpath .. " (whole file)", "" }
      vim.list_extend(lines, vim.split(c.text, "\n", { plain = true }))
    else
      local ln = current_lnum(c)
      local loc = c.nlines > 1
        and ("%s:%d-%d"):format(c.relpath, ln, ln + c.nlines - 1)
        or ("%s:%d"):format(c.relpath, ln)
      lines = { "## " .. loc, "", "```" .. (c.ft or "") }
      vim.list_extend(lines, c.code)
      lines[#lines + 1] = "```"
      lines[#lines + 1] = ""
      vim.list_extend(lines, vim.split(c.text, "\n", { plain = true }))
    end
    pages[#pages + 1] = lines
  end
  local total = #pages
  if total == 0 then return end

  local buf = vim.api.nvim_create_buf(false, true)
  vim.bo[buf].filetype = "markdown"
  vim.bo[buf].bufhidden = "wipe"

  local width = math.floor(vim.o.columns * 0.7)
  local height = math.floor(vim.o.lines * 0.6)
  local function float_cfg(title)
    return {
      relative = "editor",
      width = width,
      height = height,
      row = math.floor((vim.o.lines - height) / 2),
      col = math.floor((vim.o.columns - width) / 2),
      style = "minimal",
      border = "rounded",
      title = title,
      title_pos = "center",
    }
  end

  local idx = 1
  local win = vim.api.nvim_open_win(buf, true, float_cfg(""))
  vim.wo[win].wrap = true
  vim.wo[win].linebreak = true
  vim.wo[win].breakindent = true

  local function render()
    vim.bo[buf].modifiable = true
    vim.api.nvim_buf_set_lines(buf, 0, -1, false, pages[idx])
    vim.bo[buf].modifiable = false
    if win and vim.api.nvim_win_is_valid(win) then
      pcall(vim.api.nvim_win_set_config, win,
        float_cfg((" review %d/%d: ]/[ nav, <C-s> send all, q cancel "):format(idx, total)))
      pcall(vim.api.nvim_win_set_cursor, win, { 1, 0 })
    end
  end

  local function close()
    if win and vim.api.nvim_win_is_valid(win) then vim.api.nvim_win_close(win, true) end
    win = nil
  end
  local function send()
    close()
    deliver(build_markdown(), count)
  end
  local function nav(delta)
    idx = ((idx - 1 + delta) % total) + 1
    render()
  end

  local kopts = { buffer = buf, nowait = true, silent = true }
  vim.keymap.set("n", "]", function() nav(1) end, kopts)
  vim.keymap.set("n", "[", function() nav(-1) end, kopts)
  vim.keymap.set("n", "<Tab>", function() nav(1) end, kopts)
  vim.keymap.set("n", "<S-Tab>", function() nav(-1) end, kopts)
  vim.keymap.set("n", "<C-s>", send, kopts)
  vim.keymap.set("n", "q", close, kopts)
  vim.keymap.set("n", "<Esc>", close, kopts)

  render()
end

function M.submit()
  if not require_active() then return end
  if #M.comments == 0 and not has_message() then
    return notify.warn("nothing to send (no comments or message)")
  end
  preview_send(#M.comments)
end

function M._save_state()
  if not (M._active and M._root) then return end
  local prefix = M._root .. "/"
  ---@type string[]
  local reviewed = {}
  for file in pairs(M.reviewed) do
    if vim.startswith(file, prefix) then reviewed[#reviewed + 1] = file:sub(#prefix + 1) end
  end
  ---@type table[]
  local comments = {}
  for _, c in ipairs(M.comments) do
    comments[#comments + 1] = {
      relpath = c.relpath, ft = c.ft, lnum = current_lnum(c),
      nlines = c.nlines, code = c.code, text = c.text, file_level = c.file_level,
    }
  end
  local path = state_path(M._root)
  vim.fn.mkdir(vim.fn.fnamemodify(path, ":h"), "p")
  file_utils.write(path, vim.json.encode({ base = M._base, reviewed = reviewed, comments = comments, message = M._message }))
end

---@param root string
---@param data table
function M.resume(root, data)
  M._active = true
  M._root = root
  M._base = data.base
  M._diffed = {}
  M._total = #list_changed(root, data.base)
  M.reviewed = {}
  for _, rel in ipairs(data.reviewed or {}) do M.reviewed[root .. "/" .. rel] = true end
  M._message = data.message
  M.comments = {}
  for _, s in ipairs(data.comments or {}) do
    M.comments[#M.comments + 1] = {
      id = M._next_id,
      file = root .. "/" .. s.relpath,
      relpath = s.relpath,
      ft = s.ft,
      lnum = s.lnum,
      nlines = s.nlines,
      code = s.code,
      text = s.text,
      file_level = s.file_level,
    }
    M._next_id = M._next_id + 1
  end
  vim.cmd("Unified " .. data.base)
  M._diffed[vim.api.nvim_get_current_buf()] = true
  for _, c in ipairs(M.comments) do
    local b = vim.fn.bufnr(c.file)
    if b ~= -1 and vim.api.nvim_buf_is_loaded(b) then
      c.bufnr = b
      ensure_diff(b)
      M._render(c)
    end
  end
  notify.info(("resumed review (%d comment(s))"):format(#M.comments))
end

function M.reset()
  pcall(vim.cmd, "Unified reset")
  if M._root then clear_state(M._root) end
  M._base = nil
  M._total = 0
  M._active = false
  M._root = nil
  M._message = nil
  M._diffed = {}
  M.reviewed = {}
  M.clear()
  notify.info("review ended")
end

local aug = vim.api.nvim_create_augroup("agent_review", { clear = true })
vim.api.nvim_create_autocmd("BufWinEnter", {
  group = aug,
  callback = function(args)
    local file = vim.api.nvim_buf_get_name(args.buf)
    if is.empty(file) then return end
    ensure_diff(args.buf)
    for _, c in ipairs(M.comments) do
      if c.file == file and not (c.bufnr and vim.api.nvim_buf_is_valid(c.bufnr) and c.extmark) then
        c.bufnr = args.buf
        M._render(c)
      end
    end
  end,
})

vim.api.nvim_create_autocmd("BufWipeout", {
  group = aug,
  callback = function(args) M._diffed[args.buf] = nil end,
})

vim.api.nvim_create_autocmd("VimLeavePre", {
  group = aug,
  callback = function() M._save_state() end,
})

return M
