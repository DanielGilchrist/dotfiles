local notify = require("utils.notify")
local is = require("utils.is")
local str = require("utils.str")

local TargetPath = {}
TargetPath.__index = TargetPath

function TargetPath.new(path)
  local self = setmetatable({}, TargetPath)

  self.paths = {}

  local target_path = self:__parse_target_path(path)

  if not target_path then
    return self
  end

  table.insert(self.paths, target_path)

  local functional_target_path = self:__parse_functional_target_path(target_path)
  local integration_target_path = self:__parse_integration_target_path(target_path)
  local test_to_file_fallback_path = self:__parse_test_to_file_fallback_path(
    target_path or functional_target_path or integration_target_path
  )

  if functional_target_path then
    table.insert(self.paths, functional_target_path)
  end

  if integration_target_path then
    table.insert(self.paths, integration_target_path)
  end

  if test_to_file_fallback_path then
    table.insert(self.paths, test_to_file_fallback_path)
  end

  return self
end

function TargetPath:file()
  for _, path in ipairs(self.paths) do
    if is.file_readable(path) then
      return path
    end
  end
  return nil
end

function TargetPath:directory()
  for _, path in ipairs(self.paths) do
    local directory = path:gsub("_test", ""):gsub(".rb", "")

    if is.directory(directory) then
      return directory
    end
  end
  return nil
end

function TargetPath:__parse_target_path(path)
  if str.includes(path, "app/") then
    local result = path:gsub("app/", "test/"):gsub("%.rb$", "_test.rb")
    return result
  elseif str.includes(path, "test/") then
    local result = path:gsub("test/", "app/"):gsub("_test%.rb$", ".rb")
    return result
  end
  return nil
end

function TargetPath:__parse_functional_target_path(path)
  if str.excludes(path, "app/") and str.includes(path, "controllers/") then
    local result = path:gsub("controllers/", "functional/")
    return result
  elseif str.includes(path, "test/") and str.includes(path, "functional/") then
    local result = path:gsub("functional/", "controllers/")
    return result
  end
  return nil
end

function TargetPath:__parse_integration_target_path(path)
  if str.excludes(path, "app/") and str.includes(path, "controllers/") then
    local result = path:gsub("controllers/", "integration/")
    return result
  elseif str.includes(path, "test/") and str.includes(path, "integration/") then
    local result = path:gsub("integration/", "controllers/")
    return result
  end
  return nil
end

function TargetPath:__parse_test_to_file_fallback_path(path)
  if not path or str.excludes(path, "app/") then
    return
  end

  local base_path = path:match("(.+)/[^/]+$")
  return base_path .. ".rb"
end

return {
  open_test = function()
    local current_file_path = vim.fn.expand("%:p")
    local target_path = TargetPath.new(current_file_path)
    local target_file = target_path:file()

    if target_file then
      vim.cmd("edit " .. target_file)
      return
    end

    local target_directory = target_path:directory()

    if target_directory then
      require("oil").toggle_float(target_directory)
      return
    end

    notify.info("Unable to find corresponding file!")
  end
}
