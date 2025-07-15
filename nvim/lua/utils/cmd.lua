---Build the tanda_cli command with fallback to zsh
---@param args string[] Command arguments
---@return string # The full command string with fallbacks
local function build_tanda_cli_cmd(args)
  local command = string.format("tanda_cli %s 2>/dev/null", table.concat(args, " "))
  local zsh_command = "zsh -ic '" .. command .. "'" -- zsh is a pain in the ass
  return command .. " || " .. zsh_command .. " || " .. "echo \"tanda_cli isn't setup!\""
end

M = {}

---@class TandaCliOpts
---@field on_stdout? fun(job_id: integer, data: string[], name: string) Callback for stdout

---@param opts? TandaCliOpts Optional configuration
---@return string? # Returns command string if no on_stdout provided
M.tanda_cli = function(args, opts)
  opts = opts == nil and {} or opts
  ---@cast opts -nil

  local command = build_tanda_cli_cmd(args)

  if not opts.on_stdout then
    return command
  end

  vim.fn.jobstart(command, {
    stdout_buffered = true,
    on_stdout = opts.on_stdout,
  })
end

return M
