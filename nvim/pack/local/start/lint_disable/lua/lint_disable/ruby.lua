local notify = require("utils.notify")

return {
  disable_lint = function()
    local bufnr = vim.api.nvim_get_current_buf()
    local cursor_line = vim.api.nvim_win_get_cursor(0)[1] - 1
    local diagnostics = vim.diagnostic.get(bufnr, { lnum = cursor_line })

    if #diagnostics == 0 then
      notify.info("No diagnostics on this line")
      return
    end

    local rubocop_diagnostic = nil
    for _, diagnostic in ipairs(diagnostics) do
      if diagnostic.source == "RuboCop" and diagnostic.code then
        rubocop_diagnostic = diagnostic
        break
      end
    end

    if not rubocop_diagnostic then
      notify.error("No RuboCop diagnostic found on this line")
      return
    end

    local line_num = rubocop_diagnostic.lnum
    local line = vim.api.nvim_buf_get_lines(bufnr, line_num, line_num + 1, false)[1]
    local comment = " # rubocop:disable " .. rubocop_diagnostic.code

    vim.api.nvim_buf_set_lines(bufnr, line_num, line_num + 1, false, { line .. comment })
    notify.info("Disabled " .. rubocop_diagnostic.code)
  end
}
