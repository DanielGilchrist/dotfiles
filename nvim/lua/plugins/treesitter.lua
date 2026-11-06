return {
  "nvim-treesitter/nvim-treesitter",
  branch = "main",
  lazy = false,
  build = ":TSUpdate",
  config = function()
    local ts_path = vim.fn.stdpath("data") .. "/lazy/nvim-treesitter/runtime"
    if vim.uv.fs_stat(ts_path) then
      vim.opt.rtp:append(ts_path)
    end

    local custom_parsers = {
      crystal = {
        install_info = {
          url = "https://github.com/crystal-lang-tools/tree-sitter-crystal",
          revision = "50ca9e6fcfb16a2cbcad59203cfd8ad650e25c49",
          queries = "queries/nvim",
        },
      },
      haml = {
        install_info = {
          url = "https://github.com/vitallium/tree-sitter-haml",
          revision = "3ea15266a86dc4d921e8a2c2213d1ca15661d7ba",
          queries = "queries",
        },
      },
    }

    local function register_custom_parsers()
      local parsers = require("nvim-treesitter.parsers")
      for name, config in pairs(custom_parsers) do
        parsers[name] = config
      end
    end

    register_custom_parsers()

    vim.api.nvim_create_autocmd("User", {
      pattern = "TSUpdate",
      callback = register_custom_parsers,
    })

    local treesitter = require("nvim-treesitter")

    vim.api.nvim_create_autocmd("FileType", {
      callback = function(args)
        local lang = vim.treesitter.language.get_lang(args.match)
        local missing = lang
          and not vim.list_contains(treesitter.get_installed(), lang)
          and vim.list_contains(treesitter.get_available(), lang)

        if missing then
          treesitter.install(lang):await(function()
            vim.schedule(function()
              if vim.api.nvim_buf_is_valid(args.buf) then
                pcall(vim.treesitter.start, args.buf)
              end
            end)
          end)
          return
        end

        pcall(vim.treesitter.start, args.buf)
      end,
    })

    vim.treesitter.language.register("crystal", { "cr" })
  end,
}
