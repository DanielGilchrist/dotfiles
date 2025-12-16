local function timeout(minutes)
  return 60 * 1000 * minutes
end

return {
  "nullromo/fishtank.nvim",
  opts = {
    screensaver = {
      timeout = timeout(1),
    },
    sprite = {
      left = "<º))><",
      right = "><((º>",
      color = "#6434d4",
    },
    numberOfFish = 3
  }
}
