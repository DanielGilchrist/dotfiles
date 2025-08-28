local function disable_plugin(source)
  return { source, enabled = false }
end

return {
  disable_plugin("mfussenegger/nvim-lint"),
  disable_plugin("stevearc/conform.nvim"),
  disable_plugin("rafamadriz/friendly-snippets"),
  disable_plugin("nvim-neo-tree/neo-tree.nvim"), -- replaced by oil.nvim
  disable_plugin("monaqa/dial.nvim"),
}
