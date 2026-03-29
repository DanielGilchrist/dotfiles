local pack = require("utils.pack")

pack.add({ "https://github.com/nullromo/fishtank.nvim" })

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
