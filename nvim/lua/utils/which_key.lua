local M = {}

-- Neovim scans a buffer's mappings newest-first and starts the `timeoutlen`
-- wait as soon as it sees a longer partial match, even when an exact `nowait`
-- match (which-key's trigger) exists further down the list. So any
-- buffer-local <leader>*/]*/[* mapping created *after* which-key attached its
-- trigger (LspAttach, gitsigns on_attach, ...) delays the popup by a full
-- `timeoutlen`. Re-creating the trigger moves it back to the head of the
-- list, where `nowait` wins again. Call this after adding buffer-local
-- mappings that share a prefix with a which-key trigger.
M.refresh_triggers = function(buf)
  vim.schedule(function()
    if not vim.api.nvim_buf_is_valid(buf) then
      return
    end

    for _, mode in ipairs({ "n", "x", "o" }) do
      for _, m in ipairs(vim.api.nvim_buf_get_keymap(buf, mode)) do
        if m.desc and m.desc:find("which-key-trigger", 1, true) and m.callback then
          vim.keymap.del(mode, m.lhs, { buffer = buf })
          vim.keymap.set(mode, m.lhs, m.callback, { buffer = buf, nowait = true, desc = m.desc })
        end
      end
    end
  end)
end

return M
