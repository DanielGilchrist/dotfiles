local shit_colourschemes = {
  "^blue$",
  "^darkblue$",
  "^default$",
  "^delek$",
  "^desert$",
  "^elflord$",
  "^evening$",
  "^habamax$",
  "^industry$",
  "^koehler$",
  "^lunaperche$",
  "^morning$",
  "^murphy$",
  "^pablo$",
  "^peachpuff$",
  "^quiet$",
  "^ron$",
  "^shine$",
  "^slate$",
  "^sorbet$",
  "^torte$",
  "^vim$",
  "^wildcharm$",
  "^zaibatsu$",
  "^zellner$",
}

return {
  "ibhagwan/fzf-lua",
  keys = {
    { "<leader>sB", "<cmd>FzfLua lines<cr>", desc = "Buffers" }
  },
  opts = {
    colorschemes = { ignore_patterns = shit_colourschemes },
    files = {
      git_icons = false,
    },
    grep = {
      rg_glob = true, -- Allows filtering by filetype in grep with `foo -- *.rb` for example
    },
    oldfiles = {
      include_current_session = true,
    },
    previewers = {
      builtin = {
        syntax_limit_b = 1024 * 100 -- 100KB
      }
    }
  }
}
