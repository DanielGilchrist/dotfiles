local M = { compiler = nil, macros = {} }

local function redraw()
  vim.schedule(function()
    vim.cmd.redrawstatus()
  end)
end

function M.handlers()
  return {
    ["$/heucristic/compiler"] = function(_, params)
      M.compiler = params
      redraw()
    end,
    ["$/heucristic/macros"] = function(_, params)
      M.macros[params.target or "main"] = params
      redraw()
    end,
  }
end

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
