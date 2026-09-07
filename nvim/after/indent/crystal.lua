if vim.b.did_indent then
  return
end
vim.b.did_indent = 1

vim.bo.indentexpr = "v:lua.require'nvim-treesitter'.indentexpr()"
