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
      vim.cmd.colorscheme("evergarden")
    end,
  },
}
