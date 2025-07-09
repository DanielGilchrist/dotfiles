return {
  opts = {
    hidden = true,
    sources = {
      files = {
        hidden = true
      }
    },
    previewers = {
      file = {
        max_size = (1024 * 1024) * 3
      },
    },
    win = {
      input = {
        keys = {
          ["<c-d>"] = { "preview_scroll_down", mode = { "i", "n" } },
          ["<c-u>"] = { "preview_scroll_up", mode = { "i", "n" } },
          ["<c-f>"] = { "list_scroll_down", mode = { "i", "n" } },
          ["<c-b>"] = { "list_scroll_up", mode = { "i", "n" } },
        }
      }
    },
  },
  keys = {
    {
      "<leader>uC",
      function()
        Snacks.picker.colorschemes()
      end,
      desc = "Colorschemes"
    },
  },
}
