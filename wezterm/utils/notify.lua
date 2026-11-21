local wezterm = require("wezterm")
local os_utils = require("utils.os")

--- AppleScript double-quoted string literal with backslashes and quotes escaped.
---@param text string
---@return string
local function quoted(text)
  return '"' .. text:gsub('[\\"]', "\\%0") .. '"'
end

--- Show a desktop notification. On macOS, `window:toast_notification` requires
--- a code-signed app to actually appear, so route through osascript instead.
---@param window  Window | nil -- only used on non-macOS for toast_notification
---@param title   string
---@param content string
local function notify(window, title, content)
  if os_utils.system() == "macos" then
    local command = "display notification " .. quoted(content) .. " with title " .. quoted(title)
    wezterm.run_child_process({ "osascript", "-e", command })
  elseif window then
    window:toast_notification(title, content, nil, 4000)
  end
end

return notify
