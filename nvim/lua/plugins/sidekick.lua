return {
  "folke/sidekick.nvim",
  opts = {
    nes = { enabled = false },
    cli = {},
  },
  keys = {
    {
      "<c-.>",
      function()
        local State = require("sidekick.cli.state")
        local states = State.get({ attached = true, terminal = true })

        if #states > 0 then
          require("sidekick.cli").toggle({ name = states[1].tool.name, focus = true })
        else
          require("sidekick.cli").select({ focus = true, filter = { installed = true } })
        end
      end,
      desc = "Toggle AI CLI",
      mode = { "n", "t", "i", "x" },
    },
    { "<leader>a", "", desc = "+ai", mode = { "n", "v" } },
    {
      "<leader>aa",
      function() require("sidekick.cli").toggle() end,
      desc = "Sidekick Toggle CLI",
    },
    {
      "<leader>as",
      function() require("sidekick.cli").select({ filter = { installed = true } }) end,
      desc = "Select CLI",
    },
    {
      "<leader>ad",
      function() require("sidekick.cli").close() end,
      desc = "Detach a CLI Session",
    },
    {
      "<leader>at",
      function() require("sidekick.cli").send({ msg = "{this}" }) end,
      mode = { "x", "n" },
      desc = "Send This",
    },
    {
      "<leader>af",
      function() require("sidekick.cli").send({ msg = "{file}" }) end,
      desc = "Send File",
    },
    {
      "<leader>av",
      function() require("sidekick.cli").send({ msg = "{selection}" }) end,
      mode = { "x" },
      desc = "Send Visual Selection",
    },
    {
      "<leader>ap",
      function() require("sidekick.cli").prompt() end,
      mode = { "n", "x" },
      desc = "Sidekick Select Prompt",
    },
  },
}
