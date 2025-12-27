local function custom_plugin(name, opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend(
    "force",
    {
      lazy = true,
      event = { "CmdlineEnter" },
      dir = vim.fn.stdpath("config") .. "/lua/custom/" .. name,
      name = name,
    },
    opts
  )
end

return {
  custom_plugin("scratchpads"),
  custom_plugin("yank_test_line"),
  custom_plugin("bundle_open"),
  custom_plugin("tanda_cli"),
  custom_plugin("test_open"),
  custom_plugin("srb_tc"),
  custom_plugin("arduino_cli", { ft = "arduino" }),
}
