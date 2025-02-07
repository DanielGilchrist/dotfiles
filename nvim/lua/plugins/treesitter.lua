return {
  {
    "nvim-treesitter/nvim-treesitter",
    opts = {
      ensure_installed = {
        "ruby",
        "rust",
        "go",
        "lua",
        "javascript",
        "json",
        "html",
      },
      highlight = {
        enable = true,
      },
      indent = { enable = false },
      auto_install = false,
    },
  },
}
