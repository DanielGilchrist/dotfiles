return {
  "nvim-lualine/lualine.nvim",
  lazy = false,
  opts = {
    options = {
      section_separators = { left = "", right = "" },
      component_separators = { left = "", right = "" },
    },
    tabline = {
      lualine_a = {},
      lualine_b = { { "buffers", symbols = { alternate_file = "" } } },
      lualine_z = {},
    },
    extensions = {
      "oil",
    },
  },
}
