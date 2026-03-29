local cmd = require("utils.cmd")
local header = [[

 FISH
 ┌───────────────────────────────────────────────────────────────┐
 │         ○                                             ○       │
 │                 ><(((((((((((((º>          ○                  │
 │    ○         ○                                                │
 │       <º))))))><                        ><((((((((º>          │
 │                    ○       <º)))))><                          │
 │   ><((º>                                           ○          │
 │             ○              ○                                  │
 │                                   <º((((((((><                │
 │       ○          ><(((º>                                  ○   │
 │                                      ○                        │
 │             ○           <º))))><                ○             │
 └───────────────────────────────────────────────────────────────┘

]]

return {
  opts = {
    enabled = true,
    preset = {
      header = header,
      keys = {
        { icon = "\u{f002} ", key = "f", desc = "Find File", action = ":lua Snacks.dashboard.pick('files')" },
        { icon = "\u{f15b} ", key = "n", desc = "New File", action = ":ene | startinsert" },
        { icon = "\u{f022} ", key = "g", desc = "Find Text", action = ":lua Snacks.dashboard.pick('live_grep')" },
        { icon = "\u{f0c5} ", key = "r", desc = "Recent Files", action = ":lua Snacks.dashboard.pick('oldfiles')" },
        { icon = "\u{f423} ", key = "c", desc = "Config", action = ":lua Snacks.dashboard.pick('files', {cwd = vim.fn.stdpath('config')})" },
        { icon = "\u{e348} ", key = "s", desc = "Restore Session", action = ":lua require('persistence').load()" },
        { icon = "\u{f426} ", key = "q", desc = "Quit", action = ":qa" },
      },
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
        ttl = 60 - (os.time() % 60),
        padding = 1,
      },
    },
  },
}
