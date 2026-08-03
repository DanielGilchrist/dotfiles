-- Launch linear-tui themed from the active colorscheme, snacks.lazygit-style:
-- highlight groups are resolved to hex, written as a theme file, and passed
-- via LINEAR_TUI_THEME.
local M = {}

local theme_path = vim.fs.normalize(vim.fn.stdpath("cache") .. "/linear-tui-theme.json")
local dirty = true

vim.api.nvim_create_autocmd("ColorScheme", {
  callback = function()
    dirty = true
  end,
})

---@type table<string, { groups: string[], attr?: "fg"|"bg" }>
local slots = {
  accent = { groups = { "MatchParen" } },
  dim = { groups = { "Comment" } },
  selection_bg = { groups = { "Visual" }, attr = "bg" },
  person = { groups = { "Identifier" } },
  error = { groups = { "DiagnosticError" } },
  workspace = { groups = { "Special" } },
  group_header = { groups = { "Title" } },
  menu_header = { groups = { "Function" } },
  heading = { groups = { "@markup.heading", "Title" } },
  marker = { groups = { "Special" } },
  code = { groups = { "@markup.raw", "String" } },
  done = { groups = { "DiagnosticOk", "String" } },
  link = { groups = { "@markup.link.url", "Underlined" } },
  priority_urgent = { groups = { "DiagnosticError" } },
  priority_high = { groups = { "WarningMsg", "DiagnosticWarn" } },
  priority_medium = { groups = { "DiagnosticWarn" } },
  priority_low = { groups = { "DiagnosticInfo" } },
  state_started = { groups = { "DiagnosticWarn" } },
  state_completed = { groups = { "DiagnosticOk", "String" } },
  state_cancelled = { groups = { "DiagnosticError" } },
  state_triage = { groups = { "@keyword", "Statement" } },
}

local function resolve(slot)
  for _, group in ipairs(slot.groups) do
    local hl = vim.api.nvim_get_hl(0, { name = group, link = false })
    local value = hl[slot.attr or "fg"]

    if value then
      return string.format("#%06x", value)
    end
  end
end

function M.refresh()
  local theme = {}

  for key, slot in pairs(slots) do
    theme[key] = resolve(slot)
  end

  vim.fn.writefile({ vim.json.encode(theme) }, theme_path)
  dirty = false
end

function M.toggle()
  if dirty then
    M.refresh()
  end

  Snacks.terminal.toggle("linear", { env = { LINEAR_TUI_THEME = theme_path } })
end

return M
