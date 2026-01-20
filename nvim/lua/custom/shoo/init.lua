local cmd = require("utils.cmd")
local notify = require("utils.notify")

local function default_options(opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend(
    "force",
    {
      on_stdout = cmd.default_handler(notify.info),
      on_stderr = cmd.default_handler(notify.error),
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

  cmd.shoo({ "notification", "purge", "--force" }, default_options())
end

return M
