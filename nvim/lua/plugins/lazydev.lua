return {
  "folke/lazydev.nvim",
  event = "BufReadPost",
  dependencies = {
    "DrKJeff16/wezterm-types",
  },
  opts = {
    library = {
      { path = "${3rd}/luv/library", words = { "vim%.uv" } },
      { path = "wezterm-types", words = { "wezterm" } },
      "snacks.nvim",
    },
  },
}
