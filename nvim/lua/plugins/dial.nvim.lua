local function register_groups(opts, groups)
  for key, value in pairs(groups) do
    opts.groups[key] = opts.groups[key] or {}
    vim.list_extend(opts.groups[key], value)
  end
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

    local rails_model_associations = augend.constant.new({
      elements = { "belongs_to", "has_one", "has_many" },
      word = true,
      cyclic = true,
    })

    local ruby_class_module = augend.constant.new({
      elements = { "class", "module" },
      word = true,
      cyclic = true,
    })

    opts.dials_by_ft = vim.tbl_extend("force", opts.dials_by_ft, {
      ruby = "ruby",
    })

    register_groups(
      opts,
      {
        default = {
          http_verbs_lowercase,
        },
        ruby = {
          sorbet_sigil,
          rails_model_associations,
          ruby_class_module,
        }
      }
    )
  end
}
