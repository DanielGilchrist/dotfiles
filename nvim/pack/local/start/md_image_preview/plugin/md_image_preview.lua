local preview = function ()
  return require("md_image_preview")
end

---@type table<string, MdPreviewMode>
local modes = { float = "float", split = "split" }

vim.api.nvim_create_user_command("MdPreviewToggle", function (opts)
  local mode = modes[opts.args]
  if opts.args ~= "" and not mode then
    vim.notify("MdPreviewToggle: unknown mode " .. opts.args, vim.log.levels.ERROR)
    return
  end
  preview().toggle(mode)
end, { nargs = "?", complete = function () return { "float", "split" } end }
)

vim.api.nvim_create_user_command("MdPreviewClose", function ()
  preview().close()
end, {}
)
