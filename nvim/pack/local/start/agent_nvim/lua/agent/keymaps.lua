local config = require("agent.config")

local M = {}

---@param prefix string|nil
function M.register(prefix)
  prefix = prefix or config.default_keymap_prefix
  local agent = function() return require("agent") end

  local function map(suffix, fn, desc, mode)
    vim.keymap.set(mode or "n", prefix .. suffix, fn, { desc = desc, silent = true })
  end

  vim.keymap.set({ "n", "x" }, prefix, "", { desc = "+agent" })

  vim.keymap.set({ "n", "i", "t", "x" }, "<C-.>", function() agent().toggle() end, { desc = "agent: toggle active", silent = true })

  map("n", function() agent().new_agent() end, "new")
  map("o", function() agent().open_or_pick() end, "open/pick")
  map("s", function() agent().send_file() end, "send file ref")
  map("v", function() agent().send_visual() end, "send selection", "x")
  map("p", function() agent().send_prompt() end, "prompt")
  map("t", function() agent().switch_target() end, "switch target")
  map("k", function() agent().kill_target() end, "kill target")
end

return M
