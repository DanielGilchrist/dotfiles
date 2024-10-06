local os_utils = require "utils.os"

local key_utils = {}

function key_utils.command_key()
  return os_utils.system() == "macos" and "CMD" or "CTRL"
end

return key_utils
