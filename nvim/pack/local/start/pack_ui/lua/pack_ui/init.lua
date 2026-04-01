local pack = require("utils.pack")

local M = {}

local buf, win
local current_tab = "home"

local function close()
  if win and vim.api.nvim_win_is_valid(win) then
    vim.api.nvim_win_close(win, true)
  end
  if buf and vim.api.nvim_buf_is_valid(buf) then
    vim.api.nvim_buf_delete(buf, { force = true })
  end
  buf, win = nil, nil
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
  map("<CR>", function() M.action_update_selected() end, "Update selected")

  -- Tab navigation (Shift + key)
  map("H", function() M.show("home") end, "Home")
  map("U", function() M.action_update() end, "Update")
  map("C", function() M.action_clean() end, "Clean")
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
      vim.api.nvim_buf_add_highlight(buf, ns, hl[1], hl[2], hl[3], hl[4])
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

local function tab_bar()
  local tabs = {
    { key = "H", label = "Home", id = "home" },
    { key = "U", label = "Update", id = "update" },
    { key = "C", label = "Clean", id = "clean" },
    { key = "L", label = "Log", id = "log" },
    { key = "?", label = "Help", id = "help" },
  }

  return tabs
end

---Build the tab bar line and extmark-based highlights
---@param line_nr number
---@return string line, table[] extmarks
local function render_tab_bar(line_nr)
  local tabs = tab_bar()
  local parts = {}
  local extmarks = {}
  local col = 1 -- 0-indexed tracking

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

-- Set up highlight groups (run once)
local function setup_highlights()
  local set = vim.api.nvim_set_hl

  -- Active tab: standout bg
  set(0, "LazyButtonActive", { link = "TabLineSel" })
  -- Inactive tab: subtle bg
  set(0, "LazyButton", { link = "CursorLine" })
end

setup_highlights()

-- Re-apply on colorscheme change
vim.api.nvim_create_autocmd("ColorScheme", { callback = setup_highlights })

---Add the standard header (tab bar + separator) to lines/highlights
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

  if not buf or not vim.api.nvim_buf_is_valid(buf) then
    return
  end

  local renderer = ({
    home = M.render_home,
    log = M.render_log,
    help = M.render_help,
  })[current_tab]

  if renderer then
    renderer()
  end
end

function M.render_home()
  local plugins, orphan_set = get_plugin_data()

  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  -- Stats
  local active_count = vim.iter(plugins):filter(function(p) return p.active end):fold(0, function(a) return a + 1 end)
  local orphan_count = vim.tbl_count(orphan_set)
  local stats = ("  Total: %d plugins"):format(#plugins)
  table.insert(lines, stats)
  table.insert(lines, "")
  table.insert(highlights, { "Title", #lines - 2, 2, 9 })
  table.insert(highlights, { "Comment", #lines - 2, 9, -1 })

  if orphan_count > 0 then
    local orphan_line = ("  %d orphaned — press C to clean"):format(orphan_count)
    table.insert(lines, orphan_line)
    table.insert(lines, "")
    table.insert(highlights, { "DiagnosticError", #lines - 2, 0, -1 })
  end

  -- Loaded section
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

    -- Build version/branch info
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

    -- Highlights
    table.insert(highlights, { "DiagnosticOk", line_nr, 4, 7 })  -- dot
    local name_start = 8
    local name_end = name_start + #name
    table.insert(highlights, { "Normal", line_nr, name_start, name_end })
    -- rev
    local rev_col = name_end + #name_pad
    table.insert(highlights, { "Comment", line_nr, rev_col, rev_col + #rev })
    -- version
    local ver_col = rev_col + #rev + #rev_pad
    table.insert(highlights, { "DiagnosticInfo", line_nr, ver_col, ver_col + #ver_info })
    -- source
    table.insert(highlights, { "Comment", line_nr, ver_col + #ver_info, -1 })

    ::continue::
  end

  -- Orphaned section
  if orphan_count > 0 then
    table.insert(lines, "")
    table.insert(lines, "  Orphaned (" .. orphan_count .. ")")
    table.insert(highlights, { "DiagnosticError", #lines - 1, 0, -1 })

    for _, p in ipairs(plugins) do
      if not orphan_set[p.spec.name] then goto skip end

      local line_nr = #lines
      table.insert(lines, ("    \u{25cb} %s"):format(p.spec.name))
      table.insert(highlights, { "DiagnosticError", line_nr, 0, -1 })

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
    -- Show last 50 lines
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
    { "Navigation", "" },
    { "H", "Home — plugin list" },
    { "U", "Update all plugins" },
    { "C", "Clean orphaned plugins" },
    { "L", "Log — view pack log" },
    { "?", "Help — this screen" },
    { "", "" },
    { "Actions", "" },
    { "⏎", "Update plugin under cursor" },
    { "q / Esc", "Close" },
    { "", "" },
    { "Status Icons", "" },
    { "", "Active — loaded and running" },
    { "", "Inactive — installed but not loaded" },
    { "", "Orphan — not in config, can be cleaned" },
  }

  for _, entry in ipairs(help) do
    local key, desc = entry[1], entry[2]
    local line_nr = #lines

    if desc == "" then
      -- Section header
      if key ~= "" then
        table.insert(lines, "")
        table.insert(lines, "  " .. key)
        table.insert(highlights, { "Title", #lines - 1, 0, -1 })
      end
    else
      table.insert(lines, ("    %-12s %s"):format(key, desc))
      table.insert(highlights, { "Special", line_nr + 1, 4, 16 })
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
  M.render_home()
end

local function render_update_results(names)
  current_tab = "update"
  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  local label = names and table.concat(names, ", ") or "all plugins"
  table.insert(lines, "  Updating " .. label .. "...")
  table.insert(lines, "")
  table.insert(highlights, { "DiagnosticInfo", #lines - 2, 0, -1 })

  render(lines, highlights)

  -- Collect results via PackChanged
  local updated = {}
  local autocmd_id
  autocmd_id = vim.api.nvim_create_autocmd("PackChanged", {
    callback = function(ev)
      local kind = ev.data.kind
      if kind == "update" then
        table.insert(updated, {
          name = ev.data.spec.name,
          rev = ev.data.path and vim.fn.system("git -C " .. ev.data.path .. " rev-parse --short HEAD"):gsub("%s+", "") or "?",
        })
      end
    end,
  })

  vim.schedule(function()
    vim.pack.update(names, { force = true })

    -- Render results after update completes
    vim.schedule(function()
      vim.api.nvim_del_autocmd(autocmd_id)

      if not buf or not vim.api.nvim_buf_is_valid(buf) then return end

      lines = {}
      highlights = {}

      add_header(lines, highlights)

      if #updated == 0 then
        table.insert(lines, "  All plugins up to date")
        table.insert(highlights, { "DiagnosticOk", #lines - 1, 0, -1 })
      else
        table.insert(lines, ("  Updated %d plugin(s):"):format(#updated))
        table.insert(lines, "")
        table.insert(highlights, { "DiagnosticOk", #lines - 2, 0, -1 })

        for _, p in ipairs(updated) do
          local line_nr = #lines
          table.insert(lines, ("    %s  →  %s"):format(p.name, p.rev))
          table.insert(highlights, { "DiagnosticInfo", line_nr, 0, -1 })
        end
      end

      render(lines, highlights)
    end)
  end)
end

function M.action_update()
  render_update_results()
end

function M.action_update_selected()
  local line = vim.api.nvim_get_current_line()
  local name = line:match("[%z\1-\127\194-\253][\128-\191]*%s+(%S+)")
  if name and name ~= "Pack" and name ~= "plugins" and name ~= "─" and name ~= "plugin" then
    render_update_results({ name })
  end
end

function M.action_clean()
  current_tab = "clean"
  local orphans = pack.plugins_to_remove()

  local lines = {}
  local highlights = {}

  add_header(lines, highlights)

  if #orphans == 0 then
    table.insert(lines, "  No orphaned plugins found")
    table.insert(highlights, { "DiagnosticOk", #lines - 1, 0, -1 })
  else
    table.insert(lines, ("  Removing %d orphaned plugin(s):"):format(#orphans))
    table.insert(lines, "")
    table.insert(highlights, { "DiagnosticWarn", #lines - 2, 0, -1 })

    for _, name in ipairs(orphans) do
      table.insert(lines, "   " .. name)
      table.insert(highlights, { "DiagnosticError", #lines - 1, 0, -1 })
    end

    pack.clean()

    table.insert(lines, "")
    table.insert(lines, "  Done!")
    table.insert(highlights, { "DiagnosticOk", #lines - 1, 0, -1 })
  end

  render(lines, highlights)
end

return M
