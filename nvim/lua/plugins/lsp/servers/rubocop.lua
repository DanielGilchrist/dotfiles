return {
  server = {
    single_file_support = false,
    mason = false,
    cmd = { "env", "RUBY_YJIT_ENABLE=1", "bundle", "exec", "rubocop", "--lsp" },
  },
}
