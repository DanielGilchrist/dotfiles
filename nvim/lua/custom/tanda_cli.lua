local tanda_cli = require("../utils/cmd").tanda_cli
local notify = require("../utils/notify")
local cmd = require("../utils/cmd")

local function default_options(opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend(
    "force",
    {
      on_stdout = cmd.default_handler(notify.info),
      on_stderr = cmd.default_handler(notify.error),
    },
    opts
  )
end

local function clock_in()
  tanda_cli({ "clockin", "start", "--no-colour" }, default_options())
end

local function clock_break_start()
  tanda_cli({ "clockin", "break", "start", "--no-colour" }, default_options())
end

local function clock_break_finish()
  tanda_cli({ "clockin", "break", "finish", "--no-colour" }, default_options())
end

local function clock_out()
  tanda_cli({ "clockin", "finish", "--no-colour" }, default_options())
end

local function time_worked()
  tanda_cli({ "time_worked", "week", "--no-colour" }, default_options())
end

local function time_worked_display()
  local function spawn_window(data)
    local snacks = require("snacks")

    snacks.win({
      title = "Time worked for the week",
      text = data,
      width = 0.4,
      border = "rounded",
      bo = {
        modifiable = false,
        readonly = true,
      },
    })
  end

  tanda_cli(
    { "time_worked", "week", "--display", "--no-colour" },
    { on_stdout = cmd.default_handler(spawn_window), on_stderr = cmd.default_handler(spawn_window) }
  )
end

vim.api.nvim_create_user_command("ClockIn", clock_in, {})
vim.api.nvim_create_user_command("ClockBreakStart", clock_break_start, {})
vim.api.nvim_create_user_command("ClockBreakFinish", clock_break_finish, {})
vim.api.nvim_create_user_command("ClockOut", clock_out, {})
vim.api.nvim_create_user_command("TimeWorked", time_worked, {})
vim.api.nvim_create_user_command("TimeWorkedDisplay", time_worked_display, {})
