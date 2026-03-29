local pack = require("utils.pack")
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
