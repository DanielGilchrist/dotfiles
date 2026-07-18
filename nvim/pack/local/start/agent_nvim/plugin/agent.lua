if vim.g.loaded_agent_nvim then return end
vim.g.loaded_agent_nvim = true

require("agent.keymaps").register()

local function dispatch(action)
  local a = require("agent")
  if a[action] then a[action]() end
end

vim.api.nvim_create_user_command("AgentNew", function() dispatch("new_agent") end, {})
vim.api.nvim_create_user_command("AgentNewSession", function() dispatch("new_repo_session") end, {})
vim.api.nvim_create_user_command("AgentOpen", function() dispatch("open_or_pick") end, {})
vim.api.nvim_create_user_command("AgentPrompt", function() dispatch("send_prompt") end, {})
vim.api.nvim_create_user_command("AgentKill", function() dispatch("kill_agent") end, {})

local function review(action)
  local r = require("agent.review")
  if r[action] then r[action]() end
end

vim.api.nvim_create_user_command("AgentReview", function() review("start") end, {})
vim.api.nvim_create_user_command("AgentReviewComment", function(o)
  local range = o.range > 0 and { o.line1, o.line2 } or nil
  require("agent.review").add_comment({ range = range })
end, { range = true })
vim.api.nvim_create_user_command("AgentReviewList", function() review("list") end, {})
vim.api.nvim_create_user_command("AgentReviewSend", function() review("submit") end, {})
vim.api.nvim_create_user_command("AgentReviewReset", function() review("reset") end, {})
