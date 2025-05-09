return {
  "sindrets/diffview.nvim",
  event = { "CmdlineEnter" },
  opts = {
    view = {
      merge_tool = {
        layout = "diff3_mixed"
      }
    }
  }
}
