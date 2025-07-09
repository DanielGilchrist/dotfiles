local function each_file(files, callback)
  for _, file in ipairs(files) do
    local name = vim.fn.fnamemodify(file, ":t:r")
    local ok, config = pcall(require, "plugins.snacks." .. name)

    if ok and config then
      callback(config, name)
    end
  end
end

local function fetch_files()
  local components_dir = vim.fn.stdpath("config") .. "/lua/plugins/snacks"
  return vim.fn.glob(components_dir .. "/*.lua", false, true)
end

local function opts(files)
  local components = {}

  each_file(files, function(config, name)
    components[name] = config
  end)

  return components
end

local function keys(files)
  local keys = {}

  each_file(files, function(config)
    local config_keys = config.keys

    if config_keys then
      vim.list_extend(keys, config.keys)
    end
  end)

  return keys
end

local function config()
  local files = fetch_files()

  return {
    "folke/snacks.nvim",
    opts = opts(files),
    keys = keys(files),
  }
end

return config()
