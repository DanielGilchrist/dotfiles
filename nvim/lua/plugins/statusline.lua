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
    opts = {},
    config = function(_, opts)
      require("mini.tabline").setup(opts)
      set_tabline_highlights()
      vim.api.nvim_create_autocmd("ColorScheme", { callback = set_tabline_highlights })
    end,
  },
}
