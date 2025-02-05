return {
  "akinsho/git-conflict.nvim",
  version = "*",
  lazy = false,
  keys = {
    { "<leader>gC" },
    { "<leader>gCa", "<cmd>GitConflictChooseOurs<cr>",   desc = "Conflict - Accept current changes" },
    { "<leader>gCi", "<cmd>GitConflictChooseTheirs<cr>", desc = "Conflict - Accept incoming changes" },
    { "<leader>gCb", "<cmd>GitConflictChooseBoth<cr>",   desc = "Conflict - Accept both changes" },
    { "<leader>gCo", "<cmd>GitConflictChooseNone<cr>",   desc = "Conflict - Accept none of the changes" },
    { "<leader>gCn", "<cmd>GitConflictNextConflict<cr>", desc = "Conflict - Move to next" },
    { "<leader>gCp", "<cmd>GitConflictPrevConflict<cr>", desc = "Conflict - Move to previous" },
  },
  opts = {
    default_mappings = false,
  },
  config = true,
}
