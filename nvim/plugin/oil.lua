local pack = require("utils.pack")
pack.add({
  "https://github.com/stevearc/oil.nvim",
  "https://github.com/nvim-mini/mini.icons",
})

require("mini.icons").setup()

require("oil").setup({
  keymaps = {
    ["q"] = { "actions.close", mode = "n" },
    ["<C-d>"] = { "actions.preview_scroll_down", mode = "n" },
    ["<C-u>"] = { "actions.preview_scroll_up", mode = "n" },
  },
  float = {
    border = "rounded",
    padding = 3,
    preview = { vertical = true },
  },
  preview_win = {
    preview_method = "load",
  },
  view_options = {
    show_hidden = true,
  },
})

vim.keymap.set("n", "<leader>e", function()
  local oil = require("oil")
  if vim.w.is_oil_win then
    return oil.close()
  end
  oil.open_float(nil, { preview = { vertical = true } })
end, { desc = "Explore Files (Oil)" })
