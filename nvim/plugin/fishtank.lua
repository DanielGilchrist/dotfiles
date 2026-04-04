local pack = require("utils.pack")

pack.later({ "https://github.com/nullromo/fishtank.nvim" }, function()
  local function timeout(minutes)
    return 60 * 1000 * minutes
  end

  require("fishtank").setup({
    screensaver = {
      timeout = timeout(1),
    },
    sprite = {
      left = "<º))><",
      right = "><((º>",
      color = "#6434d4",
    },
    numberOfFish = 3,
  })
end)
