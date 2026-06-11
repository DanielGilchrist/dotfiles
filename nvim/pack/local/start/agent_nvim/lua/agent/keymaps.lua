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

  map("n", function() agent().new_agent() end, "new (worktree)")
  map("s", function() agent().new_repo_session() end, "new (repo cwd)")
  map("o", function() agent().open_or_pick() end, "open/pick")
  map("v", function() agent().send_visual() end, "comment on selection", "x")
  map("p", function() agent().send_prompt() end, "prompt")
  map("k", function() agent().kill_agent() end, "kill agent")
  map("d", function() agent().spawn_dev() end, "spawn dev server")

  -- Send a single-digit choice (1-9) to the active agent. Works for
  -- claude's Yes/No prompts (1/2) and any wider multi-choice menu.
  for i = 1, 9 do
    map(tostring(i), function() agent().send_choice(i) end, "choice " .. i)
  end
end

return M
