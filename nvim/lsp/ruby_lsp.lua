local lsp_utils = require("lsp.utils")
local is = require("utils.is")

---@class RubyDependency
---@field name       string
---@field version    string
---@field path       string
---@field dependency boolean

---@param value unknown
---@return RubyDependency?
local function decode_dependency(value)
  if not is.table(value) then return nil end
  local name, version, path = value.name, value.version, value.path
  if not (is.string(name) and is.string(version) and is.string(path)) then return nil end
  return { name = name, version = version, path = path, dependency = value.dependency == true }
end

---@param value unknown
---@return RubyDependency[]
local function decode_dependencies(value)
  local dependencies = {}
  if not is.table(value) then return dependencies end
  for _, raw in ipairs(value) do
    local dependency = decode_dependency(raw)
    if dependency then
      table.insert(dependencies, dependency)
    end
  end
  return dependencies
end

---@param client vim.lsp.Client
---@param bufnr  integer
local function add_ruby_deps_command(client, bufnr)
  ---@param opts vim.api.keyset.create_user_command.command_args
  local show_ruby_deps = function (opts)
    local params = vim.lsp.util.make_text_document_params()
    local showAll = opts.args == "all"

    client:request("rubyLsp/workspace/dependencies", params, function (error, result)
      if error then
        print("Error showing deps: " .. error.message)
        return
      end

      local qf_list = {}
      for _, item in ipairs(decode_dependencies(result)) do
        if showAll or item.dependency then
          table.insert(qf_list, {
            text = string.format("%s (%s) - %s", item.name, item.version, item.dependency),
            filename = item.path
          })
        end
      end

      vim.fn.setqflist(qf_list)
      vim.cmd("copen")
    end, bufnr
    )
  end

  vim.api.nvim_buf_create_user_command(bufnr, "ShowRubyDeps", show_ruby_deps, {
    nargs = "?",
    complete = function ()
      return { "all" }
    end
  })
end

---@param command lsp.Command
---@param _ctx    lsp.HandlerContext
local function open_file(command, _ctx)
  local args = command.arguments and command.arguments[1]
  local uri = type(args) == "table" and args[1]
  if type(uri) == "string" then
    vim.cmd("edit " .. vim.uri_to_fname(uri))
  end
end

return {
  cmd = lsp_utils.asdf_shim("ruby-lsp"),
  filetypes = { "ruby", "eruby" },
  root_markers = { "Gemfile", ".git" },
  workspace_required = true,
  cmd_env = {
    RAILS_ENV = "test"
  },
  init_options = {
    formatter = "none",
    linters = {},
    addonSettings = {
      ["Ruby LSP Rails"] = {
        enablePendingMigrationsPrompt = false
      }
    }
  },
  on_attach = function (client, bufnr)
    client.commands["rubyLsp.openFile"] = open_file
    add_ruby_deps_command(client, bufnr)
  end
}
