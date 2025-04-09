return {
  "olimorris/codecompanion.nvim",
  dependencies = {
    "nvim-lua/plenary.nvim",
    "nvim-treesitter/nvim-treesitter",
  },
  opts = {
    adapters = {
      qwen = function()
        return require("codecompanion.adapters").extend("ollama", {
          name = "qwen",
          schema = {
            model = {
              default = "qwen2.5-coder:32b",
            },
          }
        })
      end
    },
    strategies = {
      chat = {
        adapter = "qwen",
      },
      inline = {
        adapter = "qwen",
      },
      cmd = {
        adapter = "qwen",
      }
    },
  },
}
