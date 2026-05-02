local wezterm = require("wezterm")
local os_utils = require("utils.os")

---Show a desktop notification. On macOS, `window:toast_notification` requires
---a code-signed app to actually appear, so route through osascript instead.
---@param window Window|nil  -- only used on non-macOS for toast_notification
---@param title string
---@param content string
local function notify(window, title, content)
  if os_utils.system() == "macos" then
    local q = '"'
    local command = "display notification " .. q .. content .. q .. " with title " .. q .. title .. q
    wezterm.run_child_process({ "osascript", "-e", command })
  elseif window then
    window:toast_notification(title, content, nil, 4000)
  end
end

return notify
