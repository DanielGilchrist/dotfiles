local cmd = require("utils.cmd")
local notify = require("utils.notify")

---@param notification_id? string | number
---@return CliOpts
local function default_options(notification_id)
  local hide_notification = function ()
    if notification_id then
      notify.hide(notification_id)
    end
  end

  return {
    on_stdout = cmd.default_handler(function (message)
      hide_notification()
      notify.info(message)
    end),
    on_stderr = cmd.default_handler(function (message)
      hide_notification()
      notify.error(message)
    end)
  }
end

local M = {}

---@param force? boolean
M.purge = function (force)
  force = not not force

  if not force then
    error("TODO: Implement purge flow")
  end

  local notification_id = "purging-notifications-notify-id"
  notify.info("Purging notifications...", { id = notification_id, timeout = false })

  cmd.shoo({ "notification", "purge", "--force" }, default_options(notification_id))
end

return M
