---@class AgentReviewConfig
---@field base "fork"|"head"  Default diff base: the agent's fork point, or HEAD (uncommitted only)
---@field delivery "file"|"inline"  How review comments reach the agent
---@field review_file string  Filename written to the repo root in "file" delivery
---@field sign string  Gutter sign for a commented line (≤2 cells)
---@field sign_hl string  Highlight group for the gutter sign
---@field icon string  Prefix glyph on the virtual note
---@field virt_hl string  Highlight group for the virtual note
---@field range_hl string|nil  Optional highlight spanning the commented range

---@class AgentConfig
---@field worktrees_dir string Path to the worktrees parent directory
---@field default_keymap_prefix string Leader-prefixed key prefix for all bindings
---@field review AgentReviewConfig
local M = {
  worktrees_dir = vim.env.HOME .. "/worktrees",
  default_keymap_prefix = "<leader>a",
  review = {
    base = "fork",
    delivery = "file",
    review_file = ".agent-review.md",
    sign = "▌",
    sign_hl = "DiagnosticInfo",
    icon = "▌",
    virt_hl = "Comment",
    range_hl = nil,
  },
}

return M
