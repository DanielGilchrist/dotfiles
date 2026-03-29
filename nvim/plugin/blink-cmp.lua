local pack = require("utils.pack")

pack.add({
  { src = "https://github.com/saghen/blink.cmp", version = vim.version.range("1.x") },
})

require("blink.cmp").setup({
  keymap = {
    preset = "enter",
    ["<Tab>"] = { "select_next", "snippet_forward", "fallback" },
    ["<S-Tab>"] = { "select_prev", "snippet_backward", "fallback" },
    ["<Up>"] = { "fallback" },
    ["<Down>"] = { "fallback" },
    ["<Left>"] = { "fallback" },
    ["<Right>"] = { "fallback" },
  },
  completion = {
    list = {
      selection = {
        preselect = false,
        auto_insert = false,
      },
    },
    trigger = {
      show_on_insert_on_trigger_character = false,
    },
  },
})
