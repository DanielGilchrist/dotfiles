local preview = function()
  return require("md_image_preview")
end

vim.api.nvim_create_user_command("MdPreviewToggle", function(opts)
  local mode = opts.args ~= "" and opts.args or nil
  preview().toggle(mode)
end, { nargs = "?", complete = function() return { "float", "split" } end })

vim.api.nvim_create_user_command("MdPreviewClose", function()
  preview().close()
end, {})
