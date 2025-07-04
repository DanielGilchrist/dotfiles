local header = "neovim"

local notify = require("../utils/notify")
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

local function open_terminal(opts)
  opts = opts == nil and {} or opts

  local default_opts = {
    win = {
      position = "right"
    }
  }

  local merged_opts = vim.tbl_extend("force", default_opts, opts)

  Snacks.terminal(nil, merged_opts)
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
    gitbrowse = {
      notify = true,
      what = "permalink"
    },
    notifier = {
      date_format = "%I:%M%p",
      style = "fancy",
      timeout = 5000,
      top_down = false,
    },
    picker = {
      previewers = {
        file = {
          max_size = (1024 * 1024) * 3
        },
      },
      win = {
        input = {
          keys = {
            ["<c-d>"] = { "preview_scroll_down", mode = { "i", "n" } },
            ["<c-u>"] = { "preview_scroll_up", mode = { "i", "n" } },
            ["<c-f>"] = { "list_scroll_down", mode = { "i", "n" } },
            ["<c-b>"] = { "list_scroll_up", mode = { "i", "n" } },
          }
        }
      }
    },
    terminal = {},
  },
  keys = {
    -- GitBrowse
    {
      "<leader>gY",
      function()
        Snacks.gitbrowse({
          open = function(url)
            vim.fn.setreg("+", url)
            notify.info(url)
          end,
          notify = false
        })
      end,
      desc = "Git Browse (Copy)",
      mode = { "n", "x" }
    },
    -- Picker
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
    -- Terminal
    {
      "<leader>fT",
      function()
        open_terminal()
      end,
      desc = "Terminal (cwd)",
      mode = "n"
    },
    {
      "<leader>ft",
      function()
        open_terminal({ cwd = LazyVim.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = "n"
    },
    {
      "<c-/>",
      function()
        open_terminal({ cwd = LazyVim.root() })
      end,
      desc = "Terminal (Root Dir)",
      mode = "n"
    },
  }
}
