local function timeout(minutes)
  return 60 * 1000 * minutes
end

return {
  "nullromo/fishtank.nvim",
  opts = {
    screensaver = {
      timeout = timeout(1),
    },
  }
}
