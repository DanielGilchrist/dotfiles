local function local_plugin(name, opts)
  return vim.tbl_extend("force", {
    dir = vim.fn.stdpath("config") .. "/pack/local/start/" .. name,
    name = name,
    lazy = true,
  }, opts or {})
end

return {
  local_plugin("arduino_cli", { ft = "arduino" }),
  local_plugin("bundle_open", { cmd = "BundleOpen" }),
  local_plugin("lint_disable", { keys = { { "<leader>cD", function() require("lint_disable").disable_lint() end, desc = "Disable lint rule inline" } } }),
  local_plugin("scratchpads", { cmd = { "ScratchNew", "ScratchOpen", "ScratchRename", "ScratchRemove" } }),
  local_plugin("shoo", { cmd = "GHPurgeForce" }),
  local_plugin("srb_tc", { cmd = "SrbTc" }),
  local_plugin("tanda_cli", { cmd = { "ClockIn", "ClockOut", "ClockBreakStart", "ClockBreakFinish", "TimeWorked", "TimeWorkedDisplay" } }),
  local_plugin("test_open", { cmd = "TestOpen" }),
  local_plugin("yank_test_line", { cmd = { "Ytest", "Ytestn" } }),
}
