local file = require("../utils/file")
local path = require("../utils/path")
local cmd = require("../utils/cmd")

local header = "neovim"

local function default_logo()
  return [[
      ██╗      █████╗ ███████╗██╗   ██╗██╗   ██╗██╗███╗   ███╗          Z
      ██║     ██╔══██╗╚══███╔╝╚██╗ ██╔╝██║   ██║██║████╗ ████║      Z
      ██║     ███████║  ███╔╝  ╚████╔╝ ██║   ██║██║██╔████╔██║   z
      ██║     ██╔══██║ ███╔╝    ╚██╔╝  ╚██╗ ██╔╝██║██║╚██╔╝██║ z
      ███████╗██║  ██║███████╗   ██║    ╚████╔╝ ██║██║ ╚═╝ ██║
      ╚══════╝╚═╝  ╚═╝╚══════╝   ╚═╝     ╚═══╝  ╚═╝╚═╝     ╚═╝
  ]]
end

local function logo_path(header_name)
  return path.absolute_path("/plugins/logos/") .. header_name
end

local function load_header(header_name)
  return file.read(logo_path(header_name .. ".txt")) or default_logo()
end

return {
  opts = {
    enabled = true,
    preset = {
      header = load_header(header)
    },
    sections = {
      { section = "header" },
      { section = "keys",  padding = 1 },
      {
        icon = " ",
        title = "Projects",
        section = "projects",
        indent = 2,
        padding = 1,
        limit = 10,
      },
      {
        icon = "⏲",
        title = "Time Worked",
        section = "terminal",
        cmd = cmd.tanda_cli({ "time_worked", "week" }),
        padding = 1,
        random = os.time(),
      },
      { section = "startup" },
    },
  },
}
