local is = require("utils.is")

require("heucristic.build").ensure()

local function find_heucristic()
  local paths = {
    vim.fn.expand("~/.local/bin/heucristic"),
    vim.fn.expand("~/repos/DanielGilchrist/heucristic/bin/heucristic"),
  }

  for _, path in ipairs(paths) do
    if is.executable(path) then
      return path
    end
  end

  return "heucristic"
end

return {
  cmd = { find_heucristic() },
  filetypes = { "crystal" },
  root_markers = { "shard.yml", ".git" },
  handlers = require("heucristic.status").handlers(),
}
