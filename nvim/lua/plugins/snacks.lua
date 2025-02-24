local header = "neovim"

local cmd = require("../utils/cmd")
local file = require("../utils/file")
local path = require("../utils/path")

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

local shit_colourschemes = {
  "^blue$",
  "^darkblue$",
  "^default$",
  "^delek$",
  "^desert$",
  "^elflord$",
  "^evening$",
  "^habamax$",
  "^industry$",
  "^koehler$",
  "^lunaperche$",
  "^morning$",
  "^murphy$",
  "^pablo$",
  "^peachpuff$",
  "^quiet$",
  "^ron$",
  "^shine$",
  "^slate$",
  "^sorbet$",
  "^torte$",
  "^vim$",
  "^wildcharm$",
  "^zaibatsu$",
  "^zellner$",
}

-- Create logo and save in dir with
-- https://patorjk.com/software/taag/#p=display&f=ANSI%20Shadow&t=LAZYVIM
local function logo_path(header_name)
  return path.absolute_path("/plugins/logos/") .. header_name
end

local function load_header(header_name)
  return file.read(logo_path(header_name .. ".txt")) or default_logo()
end

return {
  "folke/snacks.nvim",
  opts = { -- https://github.com/folke/snacks.nvim/tree/main/docs
    dashboard = {
      enabled = true,
      preset = {
        header = load_header(header)
      },
      sections = {
        { section = "header" },
        { section = "keys",  padding = 1 },
        {
          icon = " ",
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
    notifier = {
      date_format = "%I:%M%p",
      style = "fancy",
      timeout = 5000,
      top_down = false,
    },
    terminal = {
      win = {
        position = "right"
      }
    },
  },
  keys = {
    {
      "<leader>uC",
      function()
        Snacks.picker.colorschemes({
          transform = function(item)
            for _, pattern in ipairs(shit_colourschemes) do
              if item.text:match(pattern) then
                return false
              end
            end

            return true
          end
        })
      end,
      desc = "Colorschemes"
    },
  }
}
