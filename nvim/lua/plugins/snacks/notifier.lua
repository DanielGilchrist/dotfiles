return {
  opts = {
    date_format = "%I:%M%p",
    style = "fancy",
    timeout = 5000,
    top_down = true,
  },
  keys = {
    { "<leader>sn", function() Snacks.notifier.show_history() end, desc = "Notification History" },
  },
}
