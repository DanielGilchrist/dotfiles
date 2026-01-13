return {
  {
    "nvim-treesitter/nvim-treesitter",
    opts = {
      ensure_installed = {
        "ruby",
        "rbs",
        "rust",
        "go",
        "lua",
        "javascript",
        "json",
        "html",
        "swift",
      },
      highlight = {
        enable = true,
      },
      indent = { enable = false },
      auto_install = false,
    },
  },
}
