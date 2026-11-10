return {
  "axkirillov/unified.nvim",
  cmd = "Unified",
  opts = {
    file_tree = { enabled = false },
  },
  config = function(_, opts)
    require("unified").setup(opts)
    local async = require("unified.utils.async")
    async.debounce = function(func, delay_ms)
      local timer
      return function(...)
        local args = { ... }
        if timer and not timer:is_closing() then
          timer:stop()
          timer:close()
        end
        timer = vim.defer_fn(function()
          timer = nil
          func(unpack(args))
        end, delay_ms)
      end
    end

    local git = require("unified.git")
    local diff = require("unified.diff")
    local job = require("unified.utils.job")
    local show_git_diff = git.show_git_diff_against_commit
    git.show_git_diff_against_commit = function(commit, buf)
      buf = buf or vim.api.nvim_get_current_buf()
      local path = vim.api.nvim_buf_get_name(buf)
      local root = path ~= "" and vim.fs.root(path, ".git")
      if not root or vim.fn.filereadable(path) == 1 then return show_git_diff(commit, buf) end
      local lines = vim.api.nvim_buf_get_lines(buf, 0, -1, false)
      if #lines == 1 and lines[1] == "" then return show_git_diff(commit, buf) end
      local hash, code = job.await({ "git", "rev-parse", "--verify", commit }, { cwd = root })
      if code ~= 0 or git.get_git_file_content(path, vim.trim(hash)) ~= false then return show_git_diff(commit, buf) end
      diff.display_inline_diff(buf, {
        {
          old_start = 0,
          old_count = 0,
          new_start = 1,
          new_count = #lines,
          lines = vim.tbl_map(function(line) return "+" .. line end, lines),
        },
      })
      return true
    end
  end,
}
