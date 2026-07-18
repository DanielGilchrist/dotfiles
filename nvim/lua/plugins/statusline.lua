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
    opts = {
      content = {
        active = function()
          local S = require("mini.statusline")
          local mode, mode_hl = S.section_mode({ trunc_width = 120 })
          local git = S.section_git({ trunc_width = 40 })
          local diff = S.section_diff({ trunc_width = 75 })
          local diagnostics = S.section_diagnostics({ trunc_width = 75 })
          local lsp = S.section_lsp({ trunc_width = 75 })
          local filename = S.section_filename({ trunc_width = 140 })
          local fileinfo = S.section_fileinfo({ trunc_width = 120 })
          local location = S.section_location({ trunc_width = 75 })
          local search = S.section_searchcount({ trunc_width = 75 })
          local ok, review = pcall(function() return require("agent.review").status() end)
          review = (ok and review) or ""

          return S.combine_groups({
            { hl = mode_hl, strings = { mode } },
            { hl = "MiniStatuslineDevinfo", strings = { git, diff, diagnostics, lsp } },
            "%<",
            { hl = "MiniStatuslineFilename", strings = { filename } },
            "%=",
            { hl = "MiniStatuslineDevinfo", strings = { review } },
            { hl = "MiniStatuslineFileinfo", strings = { fileinfo } },
            { hl = mode_hl, strings = { search, location } },
          })
        end,
      },
    },
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
