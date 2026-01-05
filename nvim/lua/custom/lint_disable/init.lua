local function disable_lint()
  local ft = vim.bo.filetype

  if ft == "ruby" then
    require("custom.lint_disable.ruby").disable_lint()
  else
    require("utils.notify").error("Unsupported filetype \"" .. ft .. "\" for disabling lints!")
  end
end

return {
  disable_lint = disable_lint
}
