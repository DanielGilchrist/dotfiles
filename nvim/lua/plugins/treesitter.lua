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

    require("nvim-treesitter.parsers").crystal = {
      install_info = {
        url = "https://github.com/crystal-lang-tools/tree-sitter-crystal",
        generate = false,
        generate_from_json = false,
        queries = "queries/nvim",
      },
    }

    vim.api.nvim_create_autocmd("FileType", {
      callback = function()
        pcall(vim.treesitter.start)
      end,
    })

    vim.treesitter.language.register("crystal", { "cr" })
  end,
}
