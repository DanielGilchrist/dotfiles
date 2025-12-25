local utils = require("plugins.lsp.utils")
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
  setup = function(_, opts)
    opts.on_attach = function(client)
      utils.disable_format(client)
    end
  end,
  server = {
    mason = false,
    cmd = { find_crystalline() },
  },
}
