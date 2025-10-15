return {
  "folke/sidekick.nvim",
  opts = {
    nes = {
      enabled = false
    }
  },
  keys = {
    {
      "<leader>ac",
      function()
        require("sidekick.cli").toggle({ name = "opencode", focus = true })
      end,
      desc = "Sidekick opencode toggle"
    },
  }
}
