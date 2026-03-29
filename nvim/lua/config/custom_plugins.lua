-- Custom plugins: add to rtp so their plugin/ files are auto-sourced
local config_dir = vim.fn.stdpath("config")
for _, name in ipairs({
  "scratchpads", "yank_test_line", "bundle_open", "tanda_cli",
  "test_open", "srb_tc", "arduino_cli", "lint_disable", "shoo",
}) do
  vim.opt.rtp:append(config_dir .. "/lua/custom/" .. name)
end
