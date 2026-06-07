return {
  "folke/persistence.nvim",
  event = "VeryLazy",
  opts = {},
  init = function()
    -- persistence.nvim wraps `:mks!`, which saves whatever is in
    -- `sessionoptions`. The default includes `terminal`, which saves
    -- transient `:terminal …` buffers (Snacks terminals, lazygit, the
    -- zellij agent attach, etc.). Restoring them re-spawns those processes
    -- with no args — which for `zellij` means a fresh nameless session.
    -- Strip it globally.
    vim.opt.sessionoptions:remove("terminal")
    -- Don't restore tab pages either — agent_nvim creates per-agent tabs
    -- that we don't want re-spawned (often empty) on the next launch.
    vim.opt.sessionoptions:remove("tabpages")

    vim.api.nvim_create_autocmd("User", {
      pattern = "PersistenceSavePre",
      desc = "Drop transient /tmp prompt buffers before persistence saves the session",
      callback = function()
        for _, buf in ipairs(vim.api.nvim_list_bufs()) do
          local name = vim.api.nvim_buf_get_name(buf)
          if name:match("^/tmp/agent%-prompt%-") or name:match("^/private/tmp/agent%-prompt%-") then
            pcall(vim.api.nvim_buf_delete, buf, { force = true })
          end
        end
      end,
    })
  end,
}
