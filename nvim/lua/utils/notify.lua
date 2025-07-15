local function merge_defaults(title, opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend("keep", opts, {
    title = title
  })
end

local M = {}

---Notify of information
---@param msg string
---@param opts? snacks.notify.Opts
M.info = function(msg, opts)
  Snacks.notify.info(msg, merge_defaults("Information", opts))
end

---Notify of a warning
---@param msg string
---@param opts? snacks.notify.Opts
M.warn = function(msg, opts)
  Snacks.notify.warn(msg, merge_defaults("Warning", opts))
end

---Notify of an error
---@param msg string
---@param opts? snacks.notify.Opts
M.error = function(msg, opts)
  Snacks.notify.error(msg, merge_defaults("Error", opts))
end

---Hide notification popup
---@param id number|string
M.hide = function(id)
  Snacks.notifier.hide(id)
end

return M
