---@class AgentConfig
---@field worktrees_dir string Path to the worktrees parent directory
---@field default_keymap_prefix string Leader-prefixed key prefix for all bindings
local M = {
  worktrees_dir = vim.env.HOME .. "/worktrees",
  default_keymap_prefix = "<leader>z",
}

return M
