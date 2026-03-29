local pack = require("utils.pack")

vim.api.nvim_create_autocmd("PackChanged", {
  callback = pack.handle_change,
})

vim.api.nvim_create_autocmd("VimEnter", {
  once = true,
  callback = pack.clean,
})
