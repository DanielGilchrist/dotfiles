local is = require("utils.is")

---@class heucristic.Compiler
---@field status   string
---@field errors?  integer
---@field file?    string
---@field line?    integer
---@field message? string

---@class heucristic.Macros
---@field status string

---@class heucristic.Status
---@field compiler heucristic.Compiler?
---@field macros   table<string, heucristic.Macros?>
local M = {
  macros = {}
}

local function redraw()
  vim.schedule(function ()
    vim.cmd.redrawstatus()
  end)
end

---@param params unknown
---@return heucristic.Compiler?
local function decode_compiler(params)
  if not is.table(params) then return nil end
  local status, errors, file, line, message = params.status, params.errors, params.file, params.line, params.message
  if not is.string(status) then return nil end
  if errors ~= nil and not is.integer(errors) then return nil end
  if file ~= nil and not is.string(file) then return nil end
  if line ~= nil and not is.integer(line) then return nil end
  if message ~= nil and not is.string(message) then return nil end
  return { status = status, errors = errors, file = file, line = line, message = message }
end

---@param params unknown
---@return string?, heucristic.Macros?
local function decode_macros(params)
  if not is.table(params) then return nil end
  local status, target = params.status, params.target
  if not is.string(status) then return nil end
  if target ~= nil and not is.string(target) then return nil end
  return target or "main", { status = status }
end

function M.handlers()
  return {
    ["$/heucristic/compiler"] = function (_, params)
      local compiler = decode_compiler(params)
      if not compiler then return end
      M.compiler = compiler
      redraw()
    end,
    ["$/heucristic/macros"] = function (_, params)
      local target, macros = decode_macros(params)
      if not (target and macros) then return end
      M.macros[target] = macros
      redraw()
    end
  }
end

---@param c heucristic.Compiler
---@return string
local function compiler_text(c)
  if c.status == "pending" then
    return "crystal …"
  elseif c.status == "done" then
    if c.errors and c.errors > 0 then
      return "crystal ✗ " .. c.errors
    end
    return "crystal ✓"
  elseif c.status == "failed" then
    return "crystal !"
  end
  return "crystal off"
end

function M.status()
  local parts = {}
  if M.compiler then
    parts[#parts + 1] = compiler_text(M.compiler)
  end
  local main = M.macros.main
  if main and main.status ~= "done" then
    parts[#parts + 1] = "macros " .. main.status
  end
  return table.concat(parts, " ")
end

function M.jump()
  local c = M.compiler
  if not (c and c.file) then
    vim.notify("heucristic: the compiler reported no error", vim.log.levels.INFO)
    return
  end
  vim.cmd.edit(vim.fn.fnameescape(c.file))
  if c.line then
    vim.api.nvim_win_set_cursor(0, { c.line, 0 })
  end
  vim.notify(c.message or "", vim.log.levels.ERROR)
end

vim.api.nvim_create_user_command("HeucristicError", M.jump, {})

return M
