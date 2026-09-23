local is = require("utils.is")

local header = [[

 ███████╗██╗███████╗██╗  ██╗
 ██╔════╝██║██╔════╝██║  ██║
 █████╗  ██║███████╗███████║
 ██╔══╝  ██║╚════██║██╔══██║
 ██║     ██║███████║██║  ██║
 ╚═╝     ╚═╝╚══════╝╚═╝  ╚═╝
 ┌───────────────────────────────────────────────────────────────┐
 │         ○                                             ○       │
 │                 ><(((((((((((((º>          ○                  │
 │    ○         ○                                                │
 │       <º))))))><                        ><((((((((º>          │
 │                    ○       <º)))))><                          │
 │   ><((º>                                           ○          │
 │             ○              ○                                  │
 │                                   <º))))))))><                │
 │       ○          ><(((º>                                  ○   │
 │                                      ○                        │
 │             ○           <º))))><                ○             │
 └───────────────────────────────────────────────────────────────┘
]]

local function session_buffers()
  local persistence = require("persistence")
  local session_file = persistence.current()

  if not is.file_readable(session_file) then
    session_file = persistence.current({ branch = false })
  end

  if not is.file_readable(session_file) then
    return {}
  end

  local buffers = {}
  for line in io.lines(session_file) do
    local file = line:match("^badd %+%d+ (.+)$")
    if file then
      table.insert(buffers, file)
    end
  end

  local limit = 5
  local items = {}

  for i = 1, math.min(limit, #buffers) do
    items[#items + 1] = {
      file = buffers[i],
      icon = "file",
    }
  end

  if #buffers > limit then
    items[#items + 1] = {
      text = { { "+" .. (#buffers - limit) .. " more buffers", hl = "special" } },
    }
  end

  return items
end

return {
  opts = {
    enabled = true,
    width = 60,
    preset = {
      header = header,
    },
    sections = {
      { section = "header", padding = 0 },
      { key = "q", action = ":qa", hidden = true },
      { key = "l", action = ":Lazy", hidden = true },
      { icon = "󰍛", title = "Last Session", key = "s", action = ":lua require('persistence').load()", indent = 2, padding = 1, session_buffers },
      { icon = "", title = "Projects", section = "projects", indent = 2, padding = 1, limit = 5 },
      function()
        local stats = require("lazy").stats()
        local ms = (math.floor(stats.startuptime * 100 + 0.5) / 100)
        return {
          align = "center",
          text = {
            { "⚡ Neovim loaded ", hl = "footer" },
            { stats.loaded .. "/" .. stats.count, hl = "special" },
            { " plugins in ", hl = "footer" },
            { ms .. "ms", hl = "special" },
          },
        }
      end,
    },
  },
}
