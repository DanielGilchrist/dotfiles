-- Adapted from nvim-lspconfig's lsp/tsgo.lua. Upstream only looks for `tsgo`
-- from @typescript/native-preview; since TypeScript 7 went GA the same Go
-- language server ships as `tsc`, which is what projects actually depend on.
local function server_cmd(root)
  if not root then
    return nil
  end

  local tsgo = vim.fs.joinpath(root, "node_modules/.bin/tsgo")
  if vim.fn.executable(tsgo) == 1 then
    return tsgo
  end

  local tsc = vim.fs.joinpath(root, "node_modules/.bin/tsc")
  local native = vim.fs.joinpath(root, "node_modules/typescript/lib/getExePath.js")
  if vim.fn.executable(tsc) == 1 and vim.uv.fs_stat(native) then
    return tsc
  end

  return vim.fn.executable("tsgo") == 1 and "tsgo" or nil
end

---@type vim.lsp.Config
return {
  settings = {
    typescript = {
      inlayHints = {
        parameterNames = {
          enabled = "literals",
          suppressWhenArgumentMatchesName = true,
        },
        parameterTypes = { enabled = true },
        variableTypes = { enabled = true },
        propertyDeclarationTypes = { enabled = true },
        functionLikeReturnTypes = { enabled = true },
        enumMemberValues = { enabled = true },
      },
    },
  },
  cmd = function(dispatchers, config)
    local cmd = server_cmd((config or {}).root_dir)
    return vim.lsp.rpc.start({ cmd, "--lsp", "--stdio" }, dispatchers)
  end,
  filetypes = { "javascript", "javascriptreact", "typescript", "typescriptreact" },
  root_dir = function(bufnr, on_dir)
    local root_markers = { "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lockb", "bun.lock" }
    root_markers = { root_markers, { ".git" } }

    local deno_root = vim.fs.root(bufnr, { "deno.json", "deno.jsonc" })
    local deno_lock_root = vim.fs.root(bufnr, { "deno.lock" })
    local project_root = vim.fs.root(bufnr, root_markers)

    if deno_lock_root and (not project_root or #deno_lock_root > #project_root) then
      return
    end

    if deno_root and (not project_root or #deno_root >= #project_root) then
      return
    end

    -- Projects on TypeScript <= 6 have no Go language server; ts_ls handles those
    if not server_cmd(project_root) then
      return
    end

    on_dir(project_root)
  end,
}
