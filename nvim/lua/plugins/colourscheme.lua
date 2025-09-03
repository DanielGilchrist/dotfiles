local transparent = false

local function colourscheme(source, options)
  options = options == nil and {} or options

  return vim.tbl_extend(
    "error",
    {
      source,
      event = "User LazyColorscheme"
    },
    options
  )
end

return {
  colourscheme("RRethy/base16-nvim"),
  colourscheme("everviolet/nvim", {
    name = "evergarden",
    opts = {
      theme = {
        variant = "winter", -- "winter"|"fall"|"spring"|"summer"
        -- https://github.com/everviolet/nvim/blob/4041ce92cf1387f19a8006c1e36969bfb5371c50/lua/evergarden/colors.lua#L6-L19
        accent = "green",
      },
      editor = {
        transparent_background = transparent,
        sign = { color = "none" },
        float = {
          color = "mantle",
          solid_border = false,
        },
        completion = {
          color = "surface0",
        },
      },

    }
  }),
  {
    "LazyVim/LazyVim",
    opts = {
      colorscheme = "evergarden",
    },
  },
}
