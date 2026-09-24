-- Shows a markdown buffer rendered with GitHub's stylesheet (marked.js in a
-- persistent headless Chrome) as an image in a floating or split window.
-- Chrome stays alive for the nvim session, so an edit is a ~2ms content
-- swap and every scroll step is a sub-100ms screenshot of the visible slice.
-- Chrome also reports each heading's pixel position, which is how the
-- preview follows the cursor in split mode.

local Chrome = require("md_image_preview.chrome")
local cmd_utils = require("utils.cmd")
local file_utils = require("utils.file")
local is = require("utils.is")
local notify = require("utils.notify").with_title("md preview")

---@alias MdPreviewMode "float"|"split"

---@class MdPreviewState
---@field mode MdPreviewMode
---@field src_buf integer
---@field src_win integer
---@field preview_win integer
---@field preview_buf integer
---@field float? snacks.win
---@field augroup integer
---@field placement? snacks.image.Placement
---@field content_hash? string hash of the markdown currently in the page
---@field viewport_key? string "WxH@scale" currently set in Chrome
---@field layout? MdLayout layout of the page content (CSS px)
---@field heading_lines integer[] source line of each heading
---@field offset integer scroll offset in CSS px
---@field frames string[] frame PNGs on disk, oldest first
---@field busy boolean a Chrome round-trip is in flight
---@field dirty boolean another refresh was requested while busy

---@class MdImagePreview
---@field open fun(mode?: MdPreviewMode)
---@field close fun()
---@field toggle fun(mode?: MdPreviewMode)
---@field scroll fun(lines: number)
---@field scroll_to fun(where: "top"|"bottom")
local M = {}

local CACHE_DIR = vim.fn.stdpath("cache") .. "/md_image_preview"
local FRAME_DIR = CACHE_DIR .. "/frames-" .. vim.fn.getpid()
local TEMPLATE = CACHE_DIR .. "/template.html"
local BACKGROUND = "#0d1117"
local ASSETS = {
  ["marked.min.js"] = "https://cdn.jsdelivr.net/npm/marked@15/marked.min.js",
  ["github-markdown-dark.css"] = "https://cdn.jsdelivr.net/npm/github-markdown-css@5/github-markdown-dark.css",
  ["mermaid.min.js"] = "https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.min.js",
}
local MAX_BODY_WIDTH = 1012
local PADDING_X = 40
local PADDING_Y = 32
local FRAMES_KEPT = 3
local CURSOR_ANCHOR = 1 / 3
local EDIT_DEBOUNCE_MS = 150
local FOLLOW_DEBOUNCE_MS = 30

---@type MdPreviewState|nil
local state
---@type MdChrome|nil
local browser
---Monotonic across opens: snacks caches image objects by file path, so a
---reused frame name would redisplay the previously transmitted image.
local frame_sequence = 0

---Set `vim.g.md_image_preview_debug = true` to trace renders in :messages.
local function log(...)
  if vim.g.md_image_preview_debug then
    print(("md preview %6.0fms:"):format(vim.uv.hrtime() / 1e6 % 100000), ...)
  end
end

---@return boolean
local function ensure_assets()
  vim.fn.mkdir(CACHE_DIR, "p")
  for name, url in pairs(ASSETS) do
    local target = CACHE_DIR .. "/" .. name
    if not is.file_readable(target) then
      local result = vim.system({ "curl", "-sSfL", "-o", target, url }, { text = true }):wait()
      if not cmd_utils.success(result.code) then
        notify.error("could not download " .. name .. ": " .. (result.stderr or ""))
        return false
      end
    end
  end
  return true
end

---The page that stays loaded in Chrome. `__render` swaps the markdown and
---returns the layout; `__setWidth` matches the body to the preview width.
local function write_template()
  file_utils.write(TEMPLATE, table.concat({
    "<!doctype html><html><head><meta charset=\"utf-8\">",
    ("<link rel=\"stylesheet\" href=\"file://%s/github-markdown-dark.css\">"):format(CACHE_DIR),
    ("<style>body{margin:0;background:%s}.markdown-body{box-sizing:border-box;max-width:%dpx;margin:0 auto;padding:%dpx %dpx}</style>"):format(
      BACKGROUND, MAX_BODY_WIDTH, PADDING_Y, PADDING_X
    ),
    "</head><body><div class=\"markdown-body\" id=\"content\"></div>",
    ("<script src=\"file://%s/marked.min.js\"></script>"):format(CACHE_DIR),
    ("<script src=\"file://%s/mermaid.min.js\"></script>"):format(CACHE_DIR),
    "<script>",
    "const content=document.getElementById('content');",
    "mermaid.initialize({startOnLoad:false,theme:'dark',securityLevel:'loose'});",
    "window.__setWidth=(px)=>{document.body.style.width=px+'px'};",
    "async function renderMermaid(){",
    "  const blocks=[...content.querySelectorAll('pre > code.language-mermaid')];",
    "  for(const code of blocks){",
    "    const pre=code.parentElement;const source=code.textContent;",
    "    const holder=document.createElement('pre');holder.className='mermaid';holder.textContent=source;",
    "    pre.replaceWith(holder);",
    "    try{await mermaid.run({nodes:[holder]})}catch(error){holder.textContent=source+'\\n\\n'+(error?.str||error?.message||JSON.stringify(error))}",
    "  }",
    "}",
    "window.__render=async(src)=>{content.innerHTML=marked.parse(src);await renderMermaid();",
    "const headings=[...content.querySelectorAll('h1,h2,h3,h4,h5,h6')].map(h=>Math.round(h.getBoundingClientRect().top+window.scrollY));",
    "return JSON.stringify({height:content.offsetHeight,headings})};",
    "</script></body></html>",
  }))
end

---Heading line numbers (1-based), ignoring headings inside fenced code.
---@param lines string[]
---@return integer[]
local function heading_lines(lines)
  local headings = {}
  local in_fence = false
  for index, line in ipairs(lines) do
    if line:match("^%s*```") or line:match("^%s*~~~") then
      in_fence = not in_fence
    elseif not in_fence then
      local hashes = line:match("^(#+)%s")
      if hashes and #hashes <= 6 then table.insert(headings, index) end
    end
  end
  return headings
end

---Preview window geometry. CSS px = image px / scale.
---@return { cols: integer, rows: integer, scale: integer, css_width: integer, css_height: integer, css_cell_height: number }
local function viewport()
  local size = Snacks.image.terminal.size()
  local scale = math.max(1, math.floor(size.scale + 0.5))
  local cols = vim.api.nvim_win_get_width(state.preview_win)
  local rows = vim.api.nvim_win_get_height(state.preview_win)
  return {
    cols = cols,
    rows = rows,
    scale = scale,
    css_width = math.floor(cols * size.cell_width / scale),
    css_height = math.floor(rows * size.cell_height / scale),
    css_cell_height = size.cell_height / scale,
  }
end

---Place a new frame, then drop the previous placement once the new one has
---drawn so the image never blanks between frames.
---@param png string
local function place(png)
  if not state or not vim.api.nvim_win_is_valid(state.preview_win) then return end
  local previous = state.placement
  local view = viewport()
  local swapped = false
  state.placement = Snacks.image.placement.new(state.preview_buf, png, {
    inline = false,
    max_width = view.cols,
    max_height = view.rows,
    on_update = function()
      if swapped or not previous then return end
      swapped = true
      vim.schedule(function() previous:close() end)
    end,
  })
end

---@param png_bytes string
---@return string path
local function write_frame(png_bytes)
  vim.fn.mkdir(FRAME_DIR, "p")
  frame_sequence = frame_sequence + 1
  local path = ("%s/frame-%d.png"):format(FRAME_DIR, frame_sequence)
  file_utils.write(path, png_bytes)
  table.insert(state.frames, path)
  while #state.frames > FRAMES_KEPT do
    vim.fn.delete(table.remove(state.frames, 1))
  end
  return path
end

---@return integer
local function max_offset()
  if not state or not state.layout then return 0 end
  return math.max(0, state.layout.height - viewport().css_height)
end

---Screenshot the slice at `state.offset` and place it. Serialised: a
---request while one is in flight marks the state dirty and runs after.
local function refresh()
  if not state or not state.layout or not browser then return end
  if state.busy then
    state.dirty = true
    log("refresh: busy, marked dirty")
    return
  end
  state.busy = true
  state.dirty = false
  state.offset = math.max(0, math.min(state.offset, max_offset()))

  log("capture", state.offset)
  browser:capture(state.offset, function(png, err)
    if not state then return end
    state.busy = false
    if not png then
      notify.error("screenshot failed: " .. (err or "unknown"))
      return
    end
    log("captured", #png, "bytes")
    local frame = write_frame(png)
    log("written", frame)
    place(frame)
    log("placed")
    if state.dirty then refresh() end
  end)
end

---CSS px position of source line <row>, interpolated between the nearest
---headings whose positions Chrome reported.
---@param row integer
---@param total_lines integer
---@return integer
local function line_to_px(row, total_lines)
  local layout, lines = state.layout, state.heading_lines
  local px = layout.headings
  if #lines == 0 or #lines ~= #px then
    return math.floor((row - 1) / math.max(1, total_lines - 1) * layout.height)
  end

  local from_line, from_px = 1, 0
  local to_line, to_px = total_lines + 1, layout.height
  for index, line in ipairs(lines) do
    if line <= row then
      from_line, from_px = line, px[index]
    else
      to_line, to_px = line, px[index]
      break
    end
  end

  local span = math.max(1, to_line - from_line)
  return math.floor(from_px + (row - from_line) / span * (to_px - from_px))
end

---Keep the cursor's position in the top third of the preview.
local function follow_cursor()
  if not state or not state.layout or not vim.api.nvim_win_is_valid(state.src_win) then return end
  local row = vim.api.nvim_win_get_cursor(state.src_win)[1]
  local total = vim.api.nvim_buf_line_count(state.src_buf)
  local target = line_to_px(row, total) - math.floor(viewport().css_height * CURSOR_ANCHOR)
  target = math.max(0, math.min(target, max_offset()))
  log("follow", row, "->", target, "current", state.offset)
  if target == state.offset then return end
  state.offset = target
  refresh()
end

---Push the buffer's markdown into the page if it changed, then refresh.
local function render()
  if not state or not browser then return end
  local lines = vim.api.nvim_buf_get_lines(state.src_buf, 0, -1, false)
  local markdown = table.concat(lines, "\n")
  local view = viewport()
  local viewport_key = ("%dx%d@%d"):format(view.css_width, view.css_height, view.scale)
  local hash = vim.fn.sha256(markdown .. "\n" .. viewport_key)
  log("render", hash:sub(1, 8), "cached", hash == state.content_hash)
  if hash == state.content_hash then return refresh() end

  browser:when_ready(function()
    if not state then return end
    local function with_viewport(cb)
      if state.viewport_key == viewport_key then return cb() end
      log("set_viewport", viewport_key)
      browser:set_viewport(view.css_width, view.css_height, view.scale, function(err)
        if not state then return end
        if err then
          notify.error("viewport failed: " .. err)
          return
        end
        state.viewport_key = viewport_key
        cb()
      end)
    end
    with_viewport(function()
      browser:set_content(markdown, function(layout, err)
        if not state then return end
        if not layout then
          notify.error("render failed: " .. (err or "unknown"))
          return
        end
        log("layout", layout.height, #layout.headings, "headings")
        local first = state.layout == nil
        state.content_hash = hash
        state.layout = layout
        state.heading_lines = heading_lines(lines)
        if state.mode == "split" or first then
          -- Float mode opens at the cursor so it reads as "show me this
          -- here"; afterwards it scrolls independently.
          state.offset = -1
          follow_cursor()
        else
          refresh()
        end
      end)
    end)
  end)
end

---@param lines number positive scrolls down
function M.scroll(lines)
  if not state or not state.layout then return end
  state.offset = state.offset + math.floor(lines * viewport().css_cell_height)
  refresh()
end

---@param where "top"|"bottom"
function M.scroll_to(where)
  if not state or not state.layout then return end
  state.offset = where == "top" and 0 or max_offset()
  refresh()
end

---@param buf integer
local function map_scroll_keys(buf)
  local function map(lhs, rhs)
    vim.keymap.set("n", lhs, rhs, { buffer = buf, nowait = true, silent = true })
  end
  local half = function() return math.floor(viewport().rows / 2) end
  local page = function() return viewport().rows end
  map("j", function() M.scroll(3) end)
  map("k", function() M.scroll(-3) end)
  map("<Down>", function() M.scroll(3) end)
  map("<Up>", function() M.scroll(-3) end)
  map("<C-e>", function() M.scroll(1) end)
  map("<C-y>", function() M.scroll(-1) end)
  map("<C-d>", function() M.scroll(half()) end)
  map("<C-u>", function() M.scroll(-half()) end)
  map("<C-f>", function() M.scroll(page()) end)
  map("<C-b>", function() M.scroll(-page()) end)
  map("<Space>", function() M.scroll(page()) end)
  map("gg", function() M.scroll_to("top") end)
  map("G", function() M.scroll_to("bottom") end)
  map("q", M.close)
  map("<Esc>", M.close)
end

local function attach_autocmds()
  local rerender = Snacks.util.debounce(render, { ms = EDIT_DEBOUNCE_MS })
  local follow = Snacks.util.debounce(follow_cursor, { ms = FOLLOW_DEBOUNCE_MS })

  vim.api.nvim_create_autocmd({ "TextChanged", "TextChangedI", "TextChangedP" }, {
    group = state.augroup,
    buffer = state.src_buf,
    callback = function() rerender() end,
  })
  if state.mode == "split" then
    vim.api.nvim_create_autocmd({ "CursorMoved", "CursorMovedI", "WinScrolled" }, {
      group = state.augroup,
      buffer = state.src_buf,
      callback = function() follow() end,
    })
  end
  vim.api.nvim_create_autocmd({ "BufWinLeave", "BufDelete" }, {
    group = state.augroup,
    buffer = state.src_buf,
    callback = function() M.close() end,
  })
  vim.api.nvim_create_autocmd("WinClosed", {
    group = state.augroup,
    pattern = tostring(state.preview_win),
    callback = function() M.close() end,
  })
  vim.api.nvim_create_autocmd({ "VimResized", "WinResized" }, {
    group = state.augroup,
    callback = function()
      if not state then return true end
      rerender()
    end,
  })
end

---@return integer win, integer buf, snacks.win float
local function open_float()
  local float = Snacks.win({
    relative = "editor",
    width = vim.o.columns - 4,
    height = vim.o.lines - 4,
    row = 1,
    col = 2,
    border = "rounded",
    title = " md preview ",
    title_pos = "center",
    enter = true,
    bo = { buftype = "nofile", bufhidden = "wipe" },
    wo = { number = false, relativenumber = false, signcolumn = "no" },
  })
  return float.win, float.buf, float
end

---@return integer win, integer buf
local function open_split()
  local src_win = vim.api.nvim_get_current_win()
  vim.cmd("rightbelow vsplit")
  local win = vim.api.nvim_get_current_win()
  local buf = vim.api.nvim_create_buf(false, true)
  vim.bo[buf].buftype = "nofile"
  vim.bo[buf].bufhidden = "wipe"
  vim.api.nvim_win_set_buf(win, buf)
  vim.wo[win].number = false
  vim.wo[win].relativenumber = false
  vim.wo[win].signcolumn = "no"
  vim.wo[win].winbar = " md preview "
  vim.api.nvim_set_current_win(src_win)
  return win, buf
end

local function stop_browser()
  if browser then
    browser:stop()
    browser = nil
  end
end

---@return boolean
local function ensure_browser()
  if browser and browser:alive() then return true end
  local binary = Chrome.binary()
  if not binary then
    notify.error("no Chrome/Chromium found for rendering")
    return false
  end
  write_template()
  browser = Chrome.new(binary, TEMPLATE)
  browser.on_exit = function(code)
    browser = nil
    if state then notify.error("chrome exited (" .. tostring(code) .. "), reopen the preview") end
  end
  browser:start(function(message)
    notify.error(message)
    stop_browser()
  end)
  return true
end

function M.close()
  if not state then return end
  local closing = state
  state = nil
  if closing.placement then closing.placement:close() end
  pcall(vim.api.nvim_del_augroup_by_id, closing.augroup)
  if closing.float then
    if closing.float:valid() then closing.float:close() end
  elseif vim.api.nvim_win_is_valid(closing.preview_win) then
    vim.api.nvim_win_close(closing.preview_win, true)
  end
  vim.fn.delete(FRAME_DIR, "rf")
end

---@param mode? MdPreviewMode
function M.open(mode)
  mode = mode or "float"
  if state then
    if state.mode == mode then return end
    M.close()
  end
  if vim.bo.filetype ~= "markdown" then
    notify.warn("not a markdown buffer")
    return
  end
  if not ensure_assets() then return end
  if not ensure_browser() then return end

  local src_buf = vim.api.nvim_get_current_buf()
  local src_win = vim.api.nvim_get_current_win()
  local preview_win, preview_buf, float
  if mode == "float" then
    preview_win, preview_buf, float = open_float()
  else
    preview_win, preview_buf = open_split()
  end

  state = {
    mode = mode,
    src_buf = src_buf,
    src_win = src_win,
    preview_win = preview_win,
    preview_buf = preview_buf,
    float = float,
    augroup = vim.api.nvim_create_augroup("md_image_preview", { clear = true }),
    heading_lines = {},
    offset = 0,
    frames = {},
    busy = false,
    dirty = false,
  }

  map_scroll_keys(preview_buf)
  attach_autocmds()
  render()
end

---@param mode? MdPreviewMode
function M.toggle(mode)
  mode = mode or "float"
  if state and state.mode == mode then
    M.close()
  else
    M.open(mode)
  end
end

vim.api.nvim_create_autocmd("VimLeavePre", {
  group = vim.api.nvim_create_augroup("md_image_preview_browser", { clear = true }),
  callback = function()
    M.close()
    stop_browser()
  end,
})

return M
