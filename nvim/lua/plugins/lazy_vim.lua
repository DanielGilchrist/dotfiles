return {
  "LazyVim/LazyVim",
  version = "15.*",
  keys = {
    { "<C-s",      false },
    { "<C-Up>",    false },
    { "<C-Down>",  false },
    { "<C-Left>",  false },
    { "<C-Right>", false },
    { "<A-Up>",    "<cmd>resize +2<cr>",          desc = "Increase Window Height" },
    { "<A-Down>",  "<cmd>resize -2<cr>",          desc = "Decrease Window Height" },
    { "<A-Left>",  "<cmd>vertical resize -2<cr>", desc = "Decrease Window Width" },
    { "<A-Right>", "<cmd>vertical resize +2<cr>", desc = "Increase Window Width" },
  },
}
