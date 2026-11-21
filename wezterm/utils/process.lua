---@class ProcessUtils
---@field any fun(info: LocalProcessInfo | nil, matches: fun(process: LocalProcessInfo): boolean): boolean
local M = {}

--- Whether <matches> holds for a process or any descendant of it.
---@param info    LocalProcessInfo | nil                  as from `pane:get_foreground_process_info()`
---@param matches fun(process: LocalProcessInfo): boolean
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
