local cmd = require("../utils/cmd")
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
      header = header
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
