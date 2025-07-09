local loader = require("utils.plugin_loader")

local components = {}
local keys = {}

loader.each_config("plugins/snacks", function(config, name)
  components[name] = config

  if config.keys then
    vim.list_extend(keys, config.keys)
  end
end)

return {
  "folke/snacks.nvim",
  opts = components,
  keys = keys,
}
