local function register_default(opts, ...)
  local args = { ... }
  vim.list_extend(opts.groups.default, args)
end

return {
  "monaqa/dial.nvim",
  opts = function(_, opts)
    local augend = require("dial.augend")

    local sorbet_sigil = augend.constant.new({
      elements = { "typed: ignore", "typed: false", "typed: true", "typed: strict", "typed: strong" },
      word = true,
      cyclic = true,
    })

    local http_verbs_lowercase = augend.constant.new({
      elements = { "get", "post", "patch", "put", "delete" },
      word = true,
      cyclic = true,
    })

    register_default(
      opts,
      sorbet_sigil,
      http_verbs_lowercase
    )
  end
}
