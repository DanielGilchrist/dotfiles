local os_utils = require("utils.os")

local key_utils = {}

function key_utils.command_key()
  if os_utils.system() == "macos" then
    return "CMD"
  else
    return "ALT"
  end
end

return key_utils
