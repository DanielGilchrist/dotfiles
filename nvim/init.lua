vim.loader.enable()

vim.g.mapleader = " "
vim.g.maplocalleader = "\\"

require("config.options")
require("config.filetypes")
require("config.autocmds")
require("config.keymaps")
require("config.pack")
