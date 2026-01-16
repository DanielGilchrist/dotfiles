local is = require("utils.is")

---Build a command with fallback if not setup
---@param command_name string The name of the command
---@param args string[] Command arguments
---@return string # The full command string with fallbacks
local function build_command_with_fallback(command_name, args)
  local command = string.format("%s %s 2>/dev/null", command_name, table.concat(args, " "))
  local fallback = string.format("echo \"\"%s\" isn't setup!\"", command_name)

  return command .. " || " .. fallback
end

local function validate_cli_opts(opts)
  if opts.on_stdout or opts.onstderr then
    return
  end

  error(string.format("`opts` must contain at least one of `on_stdout` or `onstderr`!"))
end

M = {}

---Create a default handler that only calls the callback when data is not empty
---@param callback fun(data: any)
---@return fun(job_id: integer, data: any)
M.default_handler = function(callback)
  return function(_, data)
    if is.not_empty(data) then
      return callback(data)
    end
  end
end

---Check if a command exit code indicates success
---@param code number
---@return boolean
M.success = function(code)
  return code == 0
end

---@class TandaCliOpts
---@field on_stdout? fun(job_id: integer, data: string[], name: string) Callback for stdout
---@field on_stderr? fun(job_id: integer, data: string[], name: string) Callback for stderr

---@param args string[] Command arguments
---@param opts? TandaCliOpts Optional configuration
---@return string? # Returns command string if no `on_stdout` or `on_stderr` provided
M.tanda_cli = function(args, opts)
  opts = opts == nil and {} or opts
  ---@cast opts -nil

  local command = build_command_with_fallback("tanda_cli", args)

  if not opts.on_stdout and not opts.on_stderr then
    return command
  end

  vim.fn.jobstart(command, {
    stdout_buffered = true,
    on_stdout = opts.on_stdout,
    on_stderr = opts.on_stderr,
  })
end

---@class ArduinoCliOpts
---@field on_stdout? fun(job_id: integer, data: string[], name: string) Callback for stdout
---@field on_stderr? fun(job_id: integer, data: string[], name: string) Callback for stderr

---@param args string[] Command arguments
---@param opts? ArduinoCliOpts Optional configuration
M.arduino_cli = function(args, opts)
  opts = opts == nil and {} or opts
  ---@cast opts -nil

  validate_cli_opts(opts)

  vim.fn.jobstart(vim.list_extend({ "arduino-cli" }, args), {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = opts.on_stdout,
    on_stderr = opts.on_stderr,
  })
end

---@class ShooCliOpts
---@field on_stdout? fun(job_id: integer, data: string[], name: string) Callback for stdout
---@field on_stderr? fun(job_id: integer, data: string[], name: string) Callback for stderr

---@param args string[] Command arguments
---@param opts? ShooCliOpts Optional configuration
M.shoo = function(args, opts)
  opts = opts == nil and {} or opts
  ---@cast opts -nil

  validate_cli_opts(opts)

  local command = build_command_with_fallback("shoo", args)

  vim.fn.jobstart(command, {
    stdout_buffered = true,
    on_stdout = opts.on_stdout,
    on_stderr = opts.on_stderr,
  })
end

return M
