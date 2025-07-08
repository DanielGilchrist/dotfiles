return {
  server = {
    single_file_support = false,
    mason = false,
    cmd = { "bundle", "exec", "rubocop", "--lsp" },
  }
}
