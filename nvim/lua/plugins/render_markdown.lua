local cmd = require("utils.cmd")
local notify = require("utils.notify")

local function fence_at_cursor(buf, lang)
  local total = vim.api.nvim_buf_line_count(buf)
  local cur = vim.api.nvim_win_get_cursor(0)[1]
  local open_pat = "^```" .. lang .. "%s*$"
  local start
  for i = cur, 1, -1 do
    local line = vim.api.nvim_buf_get_lines(buf, i - 1, i, false)[1] or ""
    if line:match(open_pat) then start = i break end
    if line:match("^```") and i ~= cur then return nil end
  end
  if not start then return nil end
  for i = start + 1, total do
    local line = vim.api.nvim_buf_get_lines(buf, i - 1, i, false)[1] or ""
    if line:match("^```%s*$") then
      local body = vim.api.nvim_buf_get_lines(buf, start, i - 1, false)
      return table.concat(body, "\n"), start, i
    end
  end
  return nil
end

local function preview_mermaid()
  local buf = vim.api.nvim_get_current_buf()
  local src = fence_at_cursor(buf, "mermaid")
  if not src then
    notify.warn("no mermaid fence at cursor")
    return
  end
  local mmd = vim.fn.tempname() .. ".mmd"
  local png = vim.fn.tempname() .. ".png"
  vim.fn.writefile(vim.split(src, "\n"), mmd)
  local out = vim.system({ "mmdr", "-i", mmd, "-o", png, "-e", "png" }, { text = true }):wait()
  if not cmd.success(out.code) then
    notify.error("mmdr failed: " .. (out.stderr or ""))
    return
  end

  local function bounds()
    return math.floor(vim.o.columns * 0.9), math.floor(vim.o.lines * 0.9)
  end
  local max_w, max_h = bounds()
  local win = Snacks.win({
    width = max_w,
    height = max_h,
    border = "rounded",
    title = " mermaid preview ",
    title_pos = "center",
    keys = { q = "close", ["<esc>"] = "close" },
    bo = { buftype = "nofile", bufhidden = "wipe" },
  })

  local placement = Snacks.image.placement.new(win.buf, png, {
    inline = false,
    max_width = max_w,
    max_height = max_h,
    on_update_pre = function(self)
      if not win:valid() then return end
      local loc = self:state().loc
      win.opts.width = loc.width
      win.opts.height = loc.height
      win:update()
    end,
  })

  local group = vim.api.nvim_create_augroup("mermaid_preview_" .. win.buf, { clear = true })
  local reflow = Snacks.util.debounce(function()
    if not win:valid() then return end
    local w, h = bounds()
    placement.opts.max_width = w
    placement.opts.max_height = h
    win.opts.width = w
    win.opts.height = h
    win:update()
    placement._state = nil
    placement:update()
  end, { ms = 150 })
  vim.api.nvim_create_autocmd({ "VimResized" }, {
    group = group,
    callback = function()
      if not win:valid() then return true end
      reflow()
    end,
  })
end

return {
  "MeanderingProgrammer/render-markdown.nvim",
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  ft = { "markdown" },
  keys = {
    { "<leader>umr", "<cmd>RenderMarkdown buf_toggle<cr>", desc = "Toggle Markdown Render" },
    { "<leader>umd", preview_mermaid, ft = "markdown", desc = "Preview mermaid fence at cursor" },
  },
  opts = {
    enabled = false,
  },
}
