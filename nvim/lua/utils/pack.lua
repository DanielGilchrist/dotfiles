local M = {}

---@type table<string, true>
local registered = {}

---Extract plugin names from a spec list
---@param specs (string|table)[]
---@return string[]
local function extract_names(specs)
  local names = {}
  for _, spec in ipairs(specs) do
    if type(spec) == "string" then
      table.insert(names, spec:match("[^/]+$") or spec)
    elseif type(spec) == "table" and spec.src then
      table.insert(names, spec.name or spec.src:match("[^/]+$") or spec.src)
    end
  end
  return names
end

---Add plugins (wrapper around vim.pack.add)
---@param specs (string|table)[]
---@param opts? table
function M.add(specs, opts)
  for _, name in ipairs(extract_names(specs)) do
    registered[name] = true
  end
  vim.pack.add(specs, opts)
end

---Add plugins deferred to after startup (via vim.schedule)
---@param specs (string|table)[]
---@param callback fun() Called after plugins are loaded
function M.later(specs, callback)
  for _, name in ipairs(extract_names(specs)) do
    registered[name] = true
  end
  vim.schedule(function()
    vim.pack.add(specs)
    callback()
  end)
end

---Get list of orphaned plugins (installed but not registered in config)
---@return string[]
function M.orphans()
  return vim.iter(vim.pack.get())
    :filter(function(p) return not registered[p.spec.name] end)
    :map(function(p) return p.spec.name end)
    :totable()
end

---Remove orphaned plugins
function M.clean()
  local orphans = M.orphans()
  if #orphans > 0 then
    vim.pack.del(orphans)
  end
end

-- Auto-clean orphans after startup
vim.api.nvim_create_autocmd("VimEnter", {
  once = true,
  callback = M.clean,
})

return M
