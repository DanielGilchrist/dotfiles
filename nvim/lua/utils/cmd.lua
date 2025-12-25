local is = require("utils.is")
---Build the tanda_cli command with fallback to zsh
---@param args string[] Command arguments
---@return string # The full command string with fallbacks
local function build_tanda_cli_cmd(args)
  local command = string.format("tanda_cli %s 2>/dev/null", table.concat(args, " "))
  local zsh_command = "zsh -ic '" .. command .. "'" -- zsh is a pain in the ass
  return command .. " || " .. zsh_command .. " || " .. "echo \"tanda_cli isn't setup!\""
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

---@param opts? TandaCliOpts Optional configuration
---@return string? # Returns command string if no `on_stdout` or `on_stderr` provided
M.tanda_cli = function(args, opts)
  opts = opts == nil and {} or opts
  ---@cast opts -nil

  local command = build_tanda_cli_cmd(args)

  if not opts.on_stdout or not opts.on_stderr then
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

  if not opts.on_stdout or not opts.on_stderr then
    return
  end

  vim.fn.jobstart(vim.list_extend({ "arduino-cli" }, args), {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = opts.on_stdout,
    on_stderr = opts.on_stderr,
  })
end

return M
