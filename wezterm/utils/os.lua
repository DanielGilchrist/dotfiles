local os_utils = {}

---@type "macos"|"linux"|"unknown"|nil
local system

---@return "macos"|"linux"|"unknown"
function os_utils.system()
	if system then return system end

	local uname = io.popen("uname"):read("*a"):gsub("\n", "")

	if uname == "Darwin" then
		system = "macos"
	elseif uname == "Linux" then
		system = "linux"
	else
		system = "unknown"
	end

	return system
end

return os_utils
