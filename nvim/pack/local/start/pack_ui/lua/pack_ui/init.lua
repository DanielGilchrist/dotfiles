local pack = require("utils.pack")

local M = {}

local buf, win
local current_tab = "home"
local status_message = nil -- { text = "...", hl = "..." }

local function close()
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
  if buf and vim.api.nvim_buf_is_valid(buf) then
    vim.api.nvim_buf_delete(buf, { force = true })
  end
  buf, win = nil, nil
  status_message = nil
end

local function create_window()
  buf = vim.api.nvim_create_buf(false, true)

  local width = math.floor(vim.o.columns * 0.8)
  local height = math.floor(vim.o.lines * 0.8)

  win = vim.api.nvim_open_win(buf, true, {
    relative = "editor",
    width = width,
    height = height,
    col = math.floor((vim.o.columns - width) / 2),
    row = math.floor((vim.o.lines - height) / 2),
    style = "minimal",
    border = "rounded",
    title = " Pack ",
    title_pos = "center",
  })

  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].modifiable = false
  vim.wo[win].cursorline = true
  vim.wo[win].wrap = false
  vim.wo[win].winblend = 0
  vim.wo[win].winhighlight = "Normal:NormalFloat,CursorLine:CursorLine"
end

local function set_keymaps()
  local map = function(key, fn, desc)
    vim.keymap.set("n", key, fn, { buf = buf, desc = desc, nowait = true })
  end

  map("q", close, "Close")
  map("<Esc>", close, "Close")

  map("H", function() M.show("home") end, "Home")
  map("U", function() M.action_update() end, "Update all")
  map("C", function() M.action_clean() end, "Clean orphans")
  map("L", function() M.show("log") end, "Log")
  map("?", function() M.show("help") end, "Help")
end

local ns = vim.api.nvim_create_namespace("pack_ui")

local function render(lines, highlights)
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false

  vim.api.nvim_buf_clear_namespace(buf, ns, 0, -1)
  if highlights then
    for _, hl in ipairs(highlights) do
      vim.hl.range(buf, ns, hl[1], { hl[2], hl[3] }, { hl[2], hl[4] })
    end
  end
end

local function get_plugin_data()
  local plugins = vim.pack.get()
  table.sort(plugins, function(a, b) return a.spec.name < b.spec.name end)

  local orphan_set = {}
  for _, name in ipairs(pack.plugins_to_remove()) do
    orphan_set[name] = true
  end

  return plugins, orphan_set
end

local function tab_bar_tabs()
  return {
    { key = "H", label = "Home", id = "home" },
    { key = "L", label = "Log", id = "log" },
    { key = "?", label = "Help", id = "help" },
  }
end

local function render_tab_bar(line_nr)
  local tabs = tab_bar_tabs()
  local parts = {}
  local extmarks = {}
  local col = 1

  for i, tab in ipairs(tabs) do
    local label = (" %s (%s) "):format(tab.label, tab.key)
    local is_active = tab.id == current_tab

    if i > 1 then
      table.insert(parts, "  ")
      col = col + 2
    end

    local start_col = col
    table.insert(parts, label)
    col = col + #label

    table.insert(extmarks, {
      is_active and "LazyButtonActive" or "LazyButton",
      line_nr,
      start_col,
      col,
    })
  end

  return " " .. table.concat(parts), extmarks
end

local function setup_highlights()
  vim.api.nvim_set_hl(0, "LazyButtonActive", { link = "TabLineSel" })
  vim.api.nvim_set_hl(0, "LazyButton", { link = "CursorLine" })
end

setup_highlights()
vim.api.nvim_create_autocmd("ColorScheme", { callback = setup_highlights })

local function add_header(lines, highlights)
  local tbar, tbar_hls = render_tab_bar(#lines)
  table.insert(lines, tbar)
  vim.list_extend(highlights, tbar_hls)
  table.insert(lines, "  " .. string.rep("─", vim.api.nvim_win_get_width(win) - 4))
  table.insert(lines, "")
  table.insert(highlights, { "Comment", #lines - 2, 0, -1 })
end

function M.show(tab)
  current_tab = tab or "home"
  if not buf or not vim.api.nvim_buf_is_valid(buf) then return end

  local renderer = ({
    home = M.render_home,
    log = M.render_log,
    help = M.render_help,
  })[current_tab]

  if renderer then renderer() end
end

function M.render_home()
  local plugins, orphan_set = get_plugin_data()

  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  -- Status message (from update/clean actions)
  if status_message then
    table.insert(lines, "  " .. status_message.text)
    table.insert(highlights, { status_message.hl, #lines - 1, 0, -1 })
    table.insert(lines, "")
  end

  -- Stats
  local active_count = vim.iter(plugins):filter(function(p) return p.active end):fold(0, function(a) return a + 1 end)
  local orphan_count = vim.tbl_count(orphan_set)

  local stats_parts = { ("Total: %d"):format(#plugins) }
  if orphan_count > 0 then
    table.insert(stats_parts, ("\u{25cb} %d orphaned"):format(orphan_count))
  end
  local stats = "  " .. table.concat(stats_parts, "  ·  ")

  table.insert(lines, stats)
  table.insert(lines, "")
  table.insert(highlights, { "Comment", #lines - 2, 0, -1 })

  -- Loaded
  table.insert(lines, "  Loaded (" .. active_count .. ")")
  table.insert(highlights, { "Title", #lines - 1, 0, -1 })

  for _, p in ipairs(plugins) do
    if not p.active or orphan_set[p.spec.name] then goto continue end

    local name = p.spec.name
    local rev = p.rev and p.rev:sub(1, 8) or "--------"
    local src = (p.spec.src or ""):match("github%.com/(.+)") or ""
    local branch = p.branches and p.branches[1] or ""
    local latest_tag = p.tags and #p.tags > 0 and p.tags[#p.tags] or nil
    local version = p.spec.version

    local ver_info = ""
    if type(version) == "string" then
      ver_info = version
    elseif latest_tag then
      ver_info = latest_tag
    elseif branch ~= "" then
      ver_info = branch
    end

    local line_nr = #lines
    local name_pad = string.rep(" ", math.max(1, 36 - #name))
    local rev_pad = string.rep(" ", math.max(1, 12 - #rev))
    local line = ("    \u{25cf} %s%s%s%s%s"):format(name, name_pad, rev, rev_pad, ver_info)

    if src ~= "" then
      line = line .. string.rep(" ", math.max(1, 70 - vim.api.nvim_strwidth(line))) .. src
    end

    table.insert(lines, line)

    table.insert(highlights, { "DiagnosticOk", line_nr, 4, 7 })
    local name_start = 8
    local name_end = name_start + #name
    table.insert(highlights, { "Normal", line_nr, name_start, name_end })
    local rev_col = name_end + #name_pad
    table.insert(highlights, { "Comment", line_nr, rev_col, rev_col + #rev })
    local ver_col = rev_col + #rev + #rev_pad
    table.insert(highlights, { "DiagnosticInfo", line_nr, ver_col, ver_col + #ver_info })
    table.insert(highlights, { "Comment", line_nr, ver_col + #ver_info, -1 })

    ::continue::
  end

  -- Orphaned
  if orphan_count > 0 then
    table.insert(lines, "")
    table.insert(lines, "  Orphaned (" .. orphan_count .. ")")
    table.insert(highlights, { "DiagnosticError", #lines - 1, 0, -1 })

    for _, p in ipairs(plugins) do
      if not orphan_set[p.spec.name] then goto skip end

      local line_nr = #lines
      local src = (p.spec.src or ""):match("github%.com/(.+)") or ""
      table.insert(lines, ("    \u{25cb} %s"):format(p.spec.name) .. (src ~= "" and ("  " .. src) or ""))
      table.insert(highlights, { "DiagnosticError", line_nr, 4, 7 })
      table.insert(highlights, { "Comment", line_nr, 7 + #p.spec.name, -1 })

      ::skip::
    end
  end

  render(lines, highlights)
end

function M.render_log()
  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  local log_path = vim.fn.stdpath("log") .. "/nvim-pack.log"
  if vim.uv.fs_stat(log_path) then
    local log_lines = vim.fn.readfile(log_path)
    local start = math.max(1, #log_lines - 50)
    for i = start, #log_lines do
      table.insert(lines, "  " .. log_lines[i])
    end
  else
    table.insert(lines, "  No log file found")
    table.insert(highlights, { "Comment", #lines - 1, 0, -1 })
  end

  render(lines, highlights)
end

function M.render_help()
  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  local help = {
    { "Tabs", "" },
    { "H", "Home — plugin list" },
    { "L", "Log — view pack log" },
    { "?", "Help — this screen" },
    { "", "" },
    { "Actions", "" },
    { "U", "Update all plugins" },
    { "C", "Clean orphaned plugins" },
    { "q / Esc", "Close" },
    { "", "" },
    { "Status", "" },
    { "\u{25cf}", "Loaded — active in session" },
    { "\u{25cb}", "Orphan — not in config, can be cleaned" },
  }

  for _, entry in ipairs(help) do
    local key, desc = entry[1], entry[2]

    if desc == "" then
      if key ~= "" then
        table.insert(lines, "")
        table.insert(lines, "  " .. key)
        table.insert(highlights, { "Title", #lines - 1, 0, -1 })
      end
    else
      table.insert(lines, ("    %-12s %s"):format(key, desc))
      table.insert(highlights, { "Special", #lines - 1, 4, 16 })
    end
  end

  render(lines, highlights)
end

function M.open()
  if win and vim.api.nvim_win_is_valid(win) then
    close()
  end

  create_window()
  set_keymaps()
  current_tab = "home"
  status_message = nil
  M.render_home()
end

function M.action_update()
  status_message = { text = "Updating...", hl = "DiagnosticInfo" }
  M.render_home()

  local updated = {}
  local autocmd_id
  autocmd_id = vim.api.nvim_create_autocmd("PackChanged", {
    callback = function(ev)
      if ev.data.kind == "update" then
        table.insert(updated, ev.data.spec.name)
      end
    end,
  })

  vim.schedule(function()
    vim.pack.update(nil, { force = true })

    vim.schedule(function()
      vim.api.nvim_del_autocmd(autocmd_id)

      if not buf or not vim.api.nvim_buf_is_valid(buf) then return end

      if #updated == 0 then
        status_message = { text = "\u{2714} All plugins up to date", hl = "DiagnosticOk" }
      else
        status_message = {
          text = ("\u{2714} Updated %d plugin(s): %s"):format(#updated, table.concat(updated, ", ")),
          hl = "DiagnosticOk",
        }
      end

      M.render_home()
    end)
  end)
end

function M.action_clean()
  local orphans = pack.plugins_to_remove()

  if #orphans == 0 then
    status_message = { text = "\u{2714} No orphaned plugins", hl = "DiagnosticOk" }
  else
    pack.clean()
    status_message = {
      text = ("\u{2714} Removed %d plugin(s): %s"):format(#orphans, table.concat(orphans, ", ")),
      hl = "DiagnosticOk",
    }
  end

  M.render_home()
end

return M
