local pack = require("utils.pack")

pack.later({ "https://github.com/echasnovski/mini.ai" }, function()
  local ai = require("mini.ai")
  local spec_treesitter = ai.gen_spec.treesitter

  ai.setup({
    custom_textobjects = {
      f = spec_treesitter({ a = "@function.outer", i = "@function.inner" }),
      c = spec_treesitter({ a = "@class.outer", i = "@class.inner" }),
      o = spec_treesitter({
        a = { "@block.outer", "@conditional.outer", "@loop.outer" },
        i = { "@block.inner", "@conditional.inner", "@loop.inner" },
      }),
      d = { "%f[%d]%d+" },
      e = {
        { "%u[%l%d]+%f[^%l%d]", "%f[%S][%l%d]+%f[^%l%d]", "%f[%P][%l%d]+%f[^%l%d]", "^[%l%d]+%f[^%l%d]" },
        "^().*()$",
      },
      g = function()
        local from = { line = 1, col = 1 }
        local to = { line = vim.fn.line("$"), col = math.max(vim.fn.getline("$"):len(), 1) }
        return { from = from, to = to }
      end,
      u = ai.gen_spec.function_call(),
      U = ai.gen_spec.function_call({ name_pattern = "[%w_]" }),
    },
  })
end)
