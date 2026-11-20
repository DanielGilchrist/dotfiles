return {
  "folke/flash.nvim",
  event = "BufReadPost",
  opts = {},
  keys = {
    { "gs", function () require("flash.commands").jump() end, mode = { "n", "x", "o" }, desc = "Flash" },
    {
      "gS",
      function () require("flash.commands").treesitter() end,
      mode = { "n", "x", "o" },
      desc = "Flash Treesitter"
    },
    { "r", function () require("flash.commands").remote() end, mode = "o", desc = "Remote Flash" },
    {
      "R",
      function () require("flash.commands").treesitter_search() end,
      mode = { "o", "x" },
      desc = "Treesitter Search"
    }
  }
}
