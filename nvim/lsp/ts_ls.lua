local default_root_dir = function(bufnr, on_dir)
  local root_markers = { "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock" }
  root_markers = { root_markers, { ".git" } }

  local deno_path = vim.fs.root(bufnr, { "deno.json", "deno.jsonc", "deno.lock" })
  local project_root = vim.fs.root(bufnr, root_markers)
  if deno_path and (not project_root or #deno_path >= #project_root) then
    return
  end

  on_dir(project_root or vim.fn.getcwd())
end

return {
  cmd = { "typescript-language-server", "--stdio" },
  filetypes = { "javascript", "javascriptreact", "typescript", "typescriptreact" },
  root_dir = function(bufnr, on_dir)
    local flow_path = vim.fs.root(bufnr, { ".flowconfig" })
    if flow_path then
      return
    end

    default_root_dir(bufnr, on_dir)
  end,
}
