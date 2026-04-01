local pack = require("utils.pack")

pack.later({ "https://github.com/folke/which-key.nvim" }, function()
  require("which-key").setup({
    spec = {
      { "<leader>b", group = "Buffer" },
      { "<leader>c", group = "Code" },
      { "<leader>f", group = "Find/File" },
      { "<leader>g", group = "Git" },
      { "<leader>gC", group = "Conflicts", icon = { icon = "", color = "orange" } },
      { "<leader>j", group = "Jump to", icon = { icon = "󰪹", color = "blue" } },
      { "<leader>m", group = "Music", icon = { icon = "", color = "green" } },
      { "<leader>q", group = "Quit" },
      { "<leader>r", group = "Arduino", icon = { icon = "", color = "blue" } },
      { "<leader>s", group = "Search" },
      { "<leader>t", group = "Test/Toggle" },
      { "<leader>u", group = "UI" },
      { "<leader>w", group = "Windows" },
      { "]", group = "Next" },
      { "[", group = "Previous" },
    },
    preset = "helix",
  })
end)
