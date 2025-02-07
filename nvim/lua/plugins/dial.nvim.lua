return {
  "monaqa/dial.nvim",
  opts = function(_, opts)
    local augend = require("dial.augend")

    local sorbet_sigil = augend.constant.new({
      elements = { "typed: ignore", "typed: false", "typed: true", "typed: strict", "typed: strong" },
      word = true,
      cyclic = true,
    })

    table.insert(opts.groups.default, sorbet_sigil)
  end
}
