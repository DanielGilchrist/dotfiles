return {
  "folke/persistence.nvim",
  event = "VeryLazy",
  opts = {},
  init = function()
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
