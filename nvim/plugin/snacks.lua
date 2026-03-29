local pack = require("utils.pack")
local loader = require("utils.plugin_loader")

pack.add({ "https://github.com/folke/snacks.nvim" })

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

require("snacks").setup(opts)

for _, key in ipairs(keys) do
  local mode = key.mode or "n"
  vim.keymap.set(mode, key[1], key[2], { desc = key.desc })
end
