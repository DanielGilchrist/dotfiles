return {
  cmd = { "bundle", "exec", "srb", "tc", "--lsp" },
  filetypes = { "ruby" },
  workspace_required = true,
  root_dir = function(bufnr, on_dir)
    local start = vim.api.nvim_buf_get_name(bufnr)
    if start == "" then start = vim.fn.getcwd() end
    for dir in vim.fs.parents(start) do
      if vim.uv.fs_stat(dir .. "/sorbet/config") then
        on_dir(dir)
        return
      end
    end
  end,
}
