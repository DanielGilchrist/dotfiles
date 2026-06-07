-- Tab page UX. Tab pages are nvim's window-layout container — distinct from
-- buffers. Each tab can have its own window layout, cwd (`:tcd`), and an
-- optional friendly name stored in `vim.t.tabname` (used by agent_nvim, and
-- the rename command below).

local M = {}

---@param tabnr integer (1-indexed tab number)
---@return string
function M.tab_label(tabnr)
  local tabid = vim.api.nvim_list_tabpages()[tabnr]
  if not tabid then return "?" end
  local explicit = vim.t[tabid].tabname
  if explicit and explicit ~= "" then return explicit end
  local cwd = vim.fn.getcwd(-1, tabnr)
  if cwd and cwd ~= "" then return vim.fn.fnamemodify(cwd, ":t") end
  return "[" .. tabnr .. "]"
end

---Build the items shown by the tab picker, one per tab page.
---@return {label: string, tabnr: integer}[]
local function tab_items()
  ---@type {label: string, tabnr: integer}[]
  local items = {}
  local current = vim.fn.tabpagenr()
  for i = 1, vim.fn.tabpagenr("$") do
    local marker = (i == current) and "▶" or " "
    table.insert(items, {
      label = string.format("%s %d  %s", marker, i, M.tab_label(i)),
      tabnr = i,
    })
  end
  return items
end

function M.pick()
  local items = tab_items()
  vim.ui.select(items, {
    prompt = "tabs",
    format_item = function(item) return item.label end,
  }, function(choice)
    if not choice then return end
    vim.cmd(("tabnext %d"):format(choice.tabnr))
  end)
end

function M.rename()
  local current = vim.fn.tabpagenr()
  local existing = vim.t.tabname or ""
  vim.ui.input({ prompt = ("rename tab %d: "):format(current), default = existing }, function(input)
    if input == nil then return end
    input = vim.trim(input)
    vim.t.tabname = input ~= "" and input or nil
    vim.cmd("redrawtabline")
  end)
end

---Register keymaps under `<leader><Tab>` and a `t` ('mnemonic for tab')
---suffix where it doesn't clash with the existing Test/Toggle group.
function M.setup()
  local prefix = "<leader><Tab>"
  local function map(suffix, fn, desc)
    vim.keymap.set("n", prefix .. suffix, fn, { desc = desc, silent = true })
  end

  vim.keymap.set("n", prefix, "", { desc = "+tabs" })
  map("n", "<cmd>tabnext<cr>", "next tab")
  map("p", "<cmd>tabprevious<cr>", "previous tab")
  map("x", "<cmd>tabclose<cr>", "close tab")
  map("o", "<cmd>tabonly<cr>", "close other tabs")
  map("t", "<cmd>tabnew<cr>", "new tab")
  map("l", M.pick, "list tabs")
  map("r", M.rename, "rename tab")
end

return M
