local M = {}

---@type table<string, true>
local registered = {}

---@type table<string, fun(ev: table)>
local hooks = {}

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

---Registers specs so we can keep track of installed plugins
---@param specs (string|table)[]
local function register(specs)
  for _, name in ipairs(extract_names(specs)) do
    registered[name] = true
  end
end

---Add plugins (wrapper around vim.pack.add)
---@param specs (string|table)[]
---@param opts? table
function M.add(specs, opts)
  register(specs)
  vim.pack.add(specs, opts)
end

---Add plugins deferred to after startup (via vim.schedule)
---@param specs (string|table)[]
---@param callback fun() Called after plugins are loaded
function M.later(specs, callback)
  register(specs)

  vim.schedule(function()
    vim.pack.add(specs)
    callback()
  end)
end

---Register a hook to run on plugin install/update
---@param name string Plugin name
---@param callback fun(ev: table) Receives the PackChanged event data
function M.on_change(name, callback)
  hooks[name] = callback
end

---Dispatch a PackChanged event to registered hooks
---@param ev table The autocmd event
function M.handle_change(ev)
  local name = ev.data.spec.name
  local kind = ev.data.kind
  if kind ~= "install" and kind ~= "update" then return end

  local hook = hooks[name]
  if hook then
    hook(ev.data)
  end
end

---Get list of plugins to remove (installed but not registered in config)
---@return string[]
function M.plugins_to_remove()
  return vim.iter(vim.pack.get())
    :filter(function(p) return not registered[p.spec.name] end)
    :map(function(p) return p.spec.name end)
    :totable()
end

---Remove plugins that aren't registered
function M.clean()
  local plugins_to_remove = M.plugins_to_remove()
  if #plugins_to_remove > 0 then
    vim.pack.del(plugins_to_remove)
  end
end

return M
