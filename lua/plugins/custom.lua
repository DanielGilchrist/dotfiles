local path = require("../utils/path")

local function load(name)
  require("custom." .. name)
end

local function register_time_worked()
  local function time_worked()
    local notify = require("../utils/notify")
    local time_worked_cmd = require("../utils/cmd").time_worked_cmd({ no_colour = true })

    local function on_stdout(_, data)
      notify.info(data)
    end

    vim.fn.jobstart(time_worked_cmd, {
      stdout_buffered = true,
      on_stdout = on_stdout,
    })
  end

  vim.api.nvim_create_user_command("TimeWorked", time_worked, {})
end

return {
  lazy = true,
  event = { "CmdlineEnter" },
  dir = path.absolute_path("/custom"),
  name = "Custom Plugins",
  config = function()
    load("scratchpads")
    load("yank_test_line")
    load("bundle_open")

    register_time_worked()
  end,
}
