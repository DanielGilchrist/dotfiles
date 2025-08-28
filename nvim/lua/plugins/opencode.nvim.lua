return {
  "NickvanDyke/opencode.nvim",
  dependencies = {
    { "folke/snacks.nvim", opts = { input = { enabled = true } } },
  },
  ---@type opencode.Opts
  opts = {},
  keys = {
    { "<leader>oA", function() require("opencode").ask() end,                                     desc = "Ask opencode", },
    { "<leader>oa", function() require("opencode").ask("@cursor: ") end,                          desc = "Ask opencode about this",      mode = "n", },
    { "<leader>oa", function() require("opencode").ask("@selection: ") end,                       desc = "Ask opencode about selection", mode = "v", },
    { "<leader>ot", function() require("opencode").toggle() end,                                  desc = "Toggle embedded opencode", },
    { "<leader>on", function() require("opencode").command("session_new") end,                    desc = "New session", },
    { "<leader>oy", function() require("opencode").command("messages_copy") end,                  desc = "Copy last message", },
    { "<leader>op", function() require("opencode").select_prompt() end,                           desc = "Select prompt",                mode = { "n", "v", }, },
    { "<leader>oe", function() require("opencode").prompt("Explain @cursor and its context") end, desc = "Explain code near cursor", },
  },
}
