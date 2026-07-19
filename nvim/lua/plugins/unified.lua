return {
  "axkirillov/unified.nvim",
  cmd = "Unified",
  opts = {
    file_tree = { enabled = false },
  },
  config = function(_, opts)
    require("unified").setup(opts)
    local async = require("unified.utils.async")
    async.debounce = function(func, delay_ms)
      local timer
      return function(...)
        local args = { ... }
        if timer and not timer:is_closing() then
          timer:stop()
          timer:close()
        end
        timer = vim.defer_fn(function()
          timer = nil
          func(unpack(args))
        end, delay_ms)
      end
    end
  end,
}
