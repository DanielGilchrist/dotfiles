return {
  "folke/sidekick.nvim",
  opts = {
    nes = { enabled = false },
  },
  keys = {
    {
      "<c-.>",
      function() require("sidekick.cli").toggle({ name = "opencode", focus = true }) end,
      desc = "Toggle OpenCode",
      mode = { "n", "t", "i", "x" },
    },
  },
}
