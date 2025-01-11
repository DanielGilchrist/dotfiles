local function build_tanda_cli_cmd(args)
  local command = string.format("tanda_cli %s 2>/dev/null", table.concat(args, " "))
  local zsh_command = "zsh -ic '" .. command .. "'" -- zsh is a pain in the ass
  return command .. " || " .. zsh_command .. " || " .. "echo \"tanda_cli isn't setup!\""
end

return {
  tanda_cli = function(args, opts)
    opts = opts == nil and {} or opts

    local command = build_tanda_cli_cmd(args)

    if not opts.on_stdout then
      return command
    end

    vim.fn.jobstart(command, {
      stdout_buffered = true,
      on_stdout = opts.on_stdout,
    })
  end
}
