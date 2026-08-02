return {
  {
    "everviolet/nvim",
    name = "evergarden",
    lazy = false,
    priority = 1000,
    opts = {
      theme = {
        variant = "winter",
        accent = "green",
      },
      editor = {
        transparent_background = false,
        sign = { color = "none" },
        float = {
          color = "mantle",
          solid_border = false,
        },
        completion = {
          color = "surface0",
        },
      },
    },
    config = function(_, opts)
      require("evergarden").setup(opts)

      local function diff_hl()
        vim.api.nvim_set_hl(0, "SnacksDiffAdd", { bg = "#22482c" })
        vim.api.nvim_set_hl(0, "SnacksDiffDelete", { bg = "#4a2530" })
      end
      vim.api.nvim_create_autocmd("ColorScheme", {
        group = vim.api.nvim_create_augroup("snacks_diff_hl", { clear = true }),
        callback = diff_hl,
      })

      vim.cmd.colorscheme("evergarden")
      diff_hl()
    end,
  },
}
