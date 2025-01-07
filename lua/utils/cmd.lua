return {
  time_worked_cmd = function(opts)
    opts = opts == nil and {} or opts
    local options_string = ""

    if opts.no_colour then
      options_string = options_string .. " --no-colour"
    end

    local command = string.format("tanda_cli time_worked week%s 2>/dev/null", options_string)
    local zsh_command = "zsh -ic '" .. command .. "'" -- zsh is a pain in the ass
    return command .. " || " .. zsh_command .. " || " .. "echo \"tanda_cli isn't setup!\""
  end
}
