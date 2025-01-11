local os_utils = {}

function os_utils.system()
  os = io.popen("uname"):read("*a"):gsub("\n", "")

  if os == "Darwin" then
    return "macos"
  elseif os == "Linux" then
    return "linux"
  else
    return "unknown"
  end
end

return os_utils
