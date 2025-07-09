local loader = require("utils.plugin_loader")

local opts = {}
local keys = {}

loader.each_config("plugins/snacks", function(config, name)
  if config.opts then
    opts[name] = config.opts
  end

  if config.keys then
    vim.list_extend(keys, config.keys)
  end
end)

return {
  "folke/snacks.nvim",
  opts = opts,
  keys = keys,
}
