local mute_notification = function(text)
  return {
    filter = {
      event = "notify",
      find = text
    },
    opts = {
      skip = true,
    }
  }
end

return {
  "folke/noice.nvim",
  opts = {
    routes = {
      mute_notification("No information available"),
    }
  }
}
