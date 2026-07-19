local function local_plugin(name, opts)
  return vim.tbl_extend("force", {
    dir = vim.fn.stdpath("config") .. "/pack/local/start/" .. name,
    name = name,
    lazy = true,
  }, opts or {})
end

return {
  local_plugin("agent_nvim", {
    keys = {
      { "<leader>a",   desc = "+agent" },
      { "<leader>an",  desc = "new (worktree)" },
      { "<leader>as",  desc = "new (repo cwd)" },
      { "<leader>ao",  desc = "open/pick" },
      { "<leader>av",  mode = "x",                         desc = "comment on selection" },
      { "<leader>ap",  desc = "prompt" },
      { "<leader>ak",  desc = "kill agent" },
      { "<leader>ar",  desc = "+review" },
      { "<leader>arr", desc = "review: start / stop" },
      { "<leader>arf", desc = "review: file picker" },
      { "<leader>arm", desc = "review: mark file reviewed" },
      { "<leader>arc", mode = { "n", "x" },                desc = "review: comment" },
      { "<leader>arl", desc = "review: jump to comment" },
      { "<leader>are", desc = "review: edit comment" },
      { "<leader>ard", desc = "review: delete comment" },
      { "<leader>ars", desc = "review: send to agent" },
      { "<C-.>",       mode = { "n", "i", "t", "x" },      desc = "agent: toggle" },
    },
    cmd = {
      "AgentNew",
      "AgentNewSession",
      "AgentOpen",
      "AgentPrompt",
      "AgentKill",
      "AgentReview",
      "AgentReviewComment",
      "AgentReviewList",
      "AgentReviewSend",
      "AgentReviewReset",
    },
  }),
  local_plugin("arduino_cli", { ft = "arduino" }),
  local_plugin("bundle_open", { cmd = "BundleOpen" }),
  local_plugin("lint_disable",
    { keys = { { "<leader>cD", function() require("lint_disable").disable_lint() end, desc = "Disable lint rule inline" } } }),
  local_plugin("scratchpads", { cmd = { "ScratchNew", "ScratchOpen", "ScratchRename", "ScratchRemove" } }),
  local_plugin("shoo", { cmd = "GHPurgeForce" }),
  local_plugin("srb_tc", { cmd = "SrbTc" }),
  local_plugin("tanda_cli",
    { cmd = { "ClockIn", "ClockOut", "ClockBreakStart", "ClockBreakFinish", "TimeWorked", "TimeWorkedDisplay" } }),
  local_plugin("test_open", { cmd = "TestOpen" }),
  local_plugin("yank_test_line", { cmd = { "Ytest", "Ytestn" } }),
}
