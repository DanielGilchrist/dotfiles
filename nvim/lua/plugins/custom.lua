local function custom_plugin(name, opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend(
    "force",
    {
      lazy = true,
      dir = vim.fn.stdpath("config") .. "/lua/custom/" .. name,
      name = name,
    },
    opts
  )
end

return {
  custom_plugin("scratchpads", { event = "CmdlineEnter" }),
  custom_plugin("yank_test_line", { event = "CmdlineEnter" }),
  custom_plugin("bundle_open", { event = "CmdlineEnter" }),
  custom_plugin("tanda_cli", { event = "CmdlineEnter" }),
  custom_plugin("test_open", { event = "CmdlineEnter" }),
  custom_plugin("srb_tc", { event = "CmdlineEnter" }),
  custom_plugin("arduino_cli", { ft = "arduino" }),
  custom_plugin("lint_disable", { ft = { "ruby" } })
}
