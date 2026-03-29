local lsp_utils = require("lsp.utils")
local is = require("utils.is")

local function find_crystalline()
  local paths = {
    "/opt/homebrew/bin/crystalline",
    "/usr/local/bin/crystalline",
  }

  for _, path in ipairs(paths) do
    if is.executable(path) then
      return path
    end
  end

  return "crystalline"
end

return {
  cmd = { find_crystalline() },
  filetypes = { "crystal" },
  root_markers = { "shard.yml", ".git" },
  on_attach = function(client)
    lsp_utils.disable_format(client)
  end,
}
