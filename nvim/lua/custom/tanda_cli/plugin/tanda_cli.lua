local tanda_cli = function()
  return require("custom.tanda_cli")
end

vim.api.nvim_create_user_command("ClockIn", function()
  tanda_cli().clock_in()
end, {})

vim.api.nvim_create_user_command("ClockBreakStart", function()
  tanda_cli().clock_break_start()
end, {})

vim.api.nvim_create_user_command("ClockBreakFinish", function()
  tanda_cli().clock_break_finish()
end, {})

vim.api.nvim_create_user_command("ClockOut", function()
  tanda_cli().clock_out()
end, {})

vim.api.nvim_create_user_command("TimeWorked", function()
  tanda_cli().time_worked()
end, {})

vim.api.nvim_create_user_command("TimeWorkedDisplay", function()
  tanda_cli().time_worked_display()
end, {})
