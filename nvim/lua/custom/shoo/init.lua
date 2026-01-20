local cmd = require("utils.cmd")
local notify = require("utils.notify")
local tbl = require("utils.table")

local function default_options(opts)
  opts = opts == nil and {} or opts

  local notification_id = tbl.remove(opts, "notification_id")

  local hide_notification = function()
    if notification_id then
      notify.hide(notification_id)
    end
  end

  return vim.tbl_extend(
    "force",
    {
      on_stdout = cmd.default_handler(function(message)
        hide_notification()
        notify.info(message)
      end),
      on_stderr = cmd.default_handler(function(message)
        hide_notification()
        notify.error(message)
      end),
    },
    opts
  )
end

local M = {}

M.purge = function(force)
  force = not not force

  if not force then
    error("TODO: Implement purge flow")
  end

  local notification_id = "purging-notifications-notify-id"
  notify.info("Purging notifications...", { id = notification_id, timeout = false })

  cmd.shoo({ "notification", "purge", "--force" }, default_options({ notification_id = notification_id }))
end

return M
