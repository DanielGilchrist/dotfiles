return {
  cmd = { "env", "RUBY_YJIT_ENABLE=1", "bundle", "exec", "rubocop", "--lsp" },
  filetypes = { "ruby" },
  root_markers = { ".rubocop.yml", ".rubocop_todo.yml" },
  workspace_required = true,
}
