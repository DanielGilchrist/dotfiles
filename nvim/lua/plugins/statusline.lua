local function set_tabline_highlights()
  vim.api.nvim_set_hl(0, "MiniTablineCurrent", { link = "TabLineSel" })
  vim.api.nvim_set_hl(0, "MiniTablineVisible", { link = "TabLine" })
  vim.api.nvim_set_hl(0, "MiniTablineHidden", { link = "TabLine" })
  vim.api.nvim_set_hl(0, "MiniTablineFill", { link = "TabLineFill" })
  vim.api.nvim_set_hl(0, "MiniTablineModifiedCurrent", { link = "TabLineSel" })
  vim.api.nvim_set_hl(0, "MiniTablineModifiedVisible", { link = "TabLine" })
  vim.api.nvim_set_hl(0, "MiniTablineModifiedHidden", { link = "TabLine" })
end

return {
  {
    "echasnovski/mini.statusline",
    opts = {},
  },
  {
    "echasnovski/mini.tabline",
    -- `tabpage_section = "right"` adds a compact tab-page indicator next to
    -- the buffer tabline so multiple vim tabs are visible at a glance
    -- (e.g. when agent_nvim opens an agent in its own tab page).
    opts = { tabpage_section = "right" },
    config = function(_, opts)
      require("mini.tabline").setup(opts)
      set_tabline_highlights()
      vim.api.nvim_create_autocmd("ColorScheme", { callback = set_tabline_highlights })
    end,
  },
}
