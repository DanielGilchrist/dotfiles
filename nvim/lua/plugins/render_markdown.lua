return {
  "MeanderingProgrammer/render-markdown.nvim",
  dependencies = { "nvim-treesitter/nvim-treesitter" },
  ft = { "markdown" },
  keys = {
    { "<leader>um", "<cmd>RenderMarkdown buf_toggle<cr>", desc = "Toggle Markdown Render" },
  },
  opts = {
    enabled = false,
  },
}
