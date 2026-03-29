local pack = require("utils.pack")

pack.add({ "https://github.com/folke/sidekick.nvim" })

require("sidekick").setup({
  nes = { enabled = false },
})

vim.keymap.set({ "n", "t", "i", "x" }, "<c-.>", function()
  require("sidekick.cli").toggle({ name = "opencode", focus = true })
end, { desc = "Toggle OpenCode" })
