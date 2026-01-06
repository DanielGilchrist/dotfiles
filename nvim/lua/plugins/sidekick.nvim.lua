local function toggle_opencode()
  require("sidekick.cli").toggle({ name = "opencode", focus = true })
end

return {
  "folke/sidekick.nvim",
  opts = {
    nes = {
      enabled = false
    }
  },
  keys = {
    {
      "<c-.>",
      toggle_opencode,
      desc = "Toggle OpenCode",
      mode = { "n", "t", "i", "x" },
    },
  }
}
