local M = {}

---Confirmation dialog using vim.fn.confirm
---@param choices string[] Button labels (e.g. {"Yes", "No"})
---@param opts {prompt: string, default?: number, type?: string} Options
---@param callback fun(choice: string|nil) Called with selected choice or nil if cancelled
function M.choice(choices, opts, callback)
  local result = vim.fn.confirm(
    opts.prompt,
    table.concat(choices, "\n"),
    opts.default or 1,
    opts.type or "Generic"
  )

  callback(result > 0 and choices[result] or nil)
end

---Simple "Ok"/"Cancel" confirmation dialog using utils.ui.choice
---@param prompt string
---@param callback fun(choice: string|nil) Called with selected choice or nil if cancelled
function M.confirm(prompt, callback)
  M.choice(
    { "Ok", "Cancel" },
    { prompt = prompt, type = "Question" },
    callback
  )
end

return M
