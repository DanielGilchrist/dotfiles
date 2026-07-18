return {
  "saghen/blink.cmp",
  dependencies = { "saghen/blink.lib" },
  event = { "InsertEnter", "CmdlineEnter" },
  build = function() require("blink.cmp").build():pwait() end,
  opts = {
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
  },
}
