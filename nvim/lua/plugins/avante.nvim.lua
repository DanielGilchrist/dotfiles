return {
  "yetone/avante.nvim",
  event = "Bufread",
   dependencies = {
    "echasnovski/mini.icons",
    "stevearc/dressing.nvim",
    "nvim-lua/plenary.nvim",
    "MunifTanjim/nui.nvim",
    {
      "MeanderingProgrammer/render-markdown.nvim",
      opts = { file_types = { "markdown", "Avante" } },
      ft = { "markdown", "Avante" },
    },
  },
  build = "make",
  opts = {
    provider = "claude",
    claude = {
      endpoint = "https://api.anthropic.com",
      model = "claude-3-7-sonnet-latest",
      temperature = 0,
      max_tokens = 4096,
    },
    behaviour = {
      auto_suggestions = false,
      enable_claude_text_editor_tool_mode = true,
    },
    ask = {
      floating = true
    }
  },
}
