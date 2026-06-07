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
  vim.keymap.set({ "n", "i", "t", "x" }, "<C-,>", function() agent().toggle_composer() end, { desc = "agent: toggle composer", silent = true })

  map("n", function() agent().new_agent() end, "new (worktree)")
  map("s", function() agent().new_repo_session() end, "new (repo cwd)")
  map("o", function() agent().open_or_pick() end, "open/pick")
  map("v", function() agent().send_visual() end, "comment on selection", "x")
  map("p", function() agent().send_prompt() end, "prompt")
  map("k", function() agent().kill_agent() end, "kill agent")
end

return M
