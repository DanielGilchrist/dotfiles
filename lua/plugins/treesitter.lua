return {
  {
    "nvim-treesitter/nvim-treesitter",
    build = ":TSUpdate",
    config = function()
      local configs = require("nvim-treesitter.configs")

      configs.setup({
        ensure_installed = {
          "ruby",
          "rust",
          "go",
          "lua",
          "javascript",
          "json",
          "html",
        },
        sync_install = false,
        highlight = { enable = true },
        indent = { enable = false },
        auto_install = false,
        ignore_install = {},
      })
    end,
  },
}
