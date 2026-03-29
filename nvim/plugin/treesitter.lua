local pack = require("utils.pack")

pack.on_change("nvim-treesitter", function(data)
  if not data.active then vim.cmd.packadd("nvim-treesitter") end
  vim.cmd("TSUpdate")
end)

pack.add({
  { src = "https://github.com/nvim-treesitter/nvim-treesitter", version = "main" },
})

-- Register crystal parser
require("nvim-treesitter.parsers").crystal = {
  install_info = {
    url = "https://github.com/crystal-lang-tools/tree-sitter-crystal",
    generate = false,
    generate_from_json = false,
    queries = "queries/nvim",
  },
}

-- Enable treesitter highlighting for all filetypes with an installed parser
vim.api.nvim_create_autocmd("FileType", {
  callback = function()
    pcall(vim.treesitter.start)
  end,
})
