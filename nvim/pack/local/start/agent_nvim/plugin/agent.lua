if vim.g.loaded_agent_nvim then return end
vim.g.loaded_agent_nvim = true

require("agent.keymaps").register()

local function dispatch(action)
  local a = require("agent")
  if a[action] then a[action]() end
end

vim.api.nvim_create_user_command("AgentNew", function() dispatch("new_agent") end, {})
vim.api.nvim_create_user_command("AgentOpen", function() dispatch("open_or_pick") end, {})
vim.api.nvim_create_user_command("AgentSend", function() dispatch("send_file") end, {})
vim.api.nvim_create_user_command("AgentPrompt", function() dispatch("send_prompt") end, {})
vim.api.nvim_create_user_command("AgentSwitch", function() dispatch("switch_target") end, {})
vim.api.nvim_create_user_command("AgentKill", function() dispatch("kill_target") end, {})
