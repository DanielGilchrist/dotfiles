---@class ProcessUtils
---@field any fun(info: table|nil, matches: fun(process: table): boolean): boolean
local M = {}

---Whether <matches> holds for a process or any descendant of it.
---@param info table|nil LocalProcessInfo, as from `pane:get_foreground_process_info()`
---@param matches fun(process: table): boolean
---@return boolean
function M.any(info, matches)
  if not info then return false end
  if matches(info) then return true end
  for _, child in pairs(info.children or {}) do
    if M.any(child, matches) then return true end
  end
  return false
end

return M
