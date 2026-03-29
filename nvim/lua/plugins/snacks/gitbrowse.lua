local notify = require("utils.notify")

return {
  opts = {
    notify = true,
    what = "permalink",
  },
  keys = {
    {
      "<leader>gB",
      function() Snacks.gitbrowse() end,
      desc = "Git Browse",
      mode = { "n", "x" }
    },
    {
      "<leader>gY",
      function()
        Snacks.gitbrowse({
          open = function(url)
            vim.fn.setreg("+", url)
            notify.info(url)
          end,
          notify = false
        })
      end,
      desc = "Git Browse (Copy)",
      mode = { "n", "x" }
    },
  },
}
