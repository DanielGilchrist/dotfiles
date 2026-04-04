return {
  cmd = { "env", "RUBY_YJIT_ENABLE=1", "bundle", "exec", "rubocop", "--lsp" },
  filetypes = { "ruby" },
  root_markers = { "Gemfile", ".git" },
  single_file_support = false,
}
