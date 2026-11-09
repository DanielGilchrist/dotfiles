local M = {}

local repo = vim.fn.expand("~/repos/DanielGilchrist/heucristic")
local building = false

local function installed()
  local file = io.open(repo .. "/bin/.installed-commit", "r")
  if not file then
    return nil
  end
  local commit = vim.trim(file:read("*a"))
  file:close()
  return commit
end

local function restart()
  if #vim.lsp.get_clients({ name = "heucristic" }) > 0 then
    vim.cmd("lsp restart heucristic")
    return
  end
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].filetype == "crystal" and not vim.bo[buf].modified then
      vim.api.nvim_buf_call(buf, function()
        vim.cmd("edit")
      end)
    end
  end
end

function M.ensure()
  if building or vim.fn.isdirectory(repo) == 0 then
    return
  end

  building = true
  local before = installed()
  local output = {}
  local function collect(_, data)
    if not data then
      return
    end
    if #output == 0 then
      vim.schedule(function()
        vim.notify("heucristic: building in the background")
      end)
    end
    table.insert(output, data)
  end
  vim.system({ "script/install", "--if-changed" }, { cwd = repo, text = true, stdout = collect }, function(result)
    vim.schedule(function()
      building = false
      if result.code ~= 0 then
        local lines = vim.split(vim.trim(result.stderr ~= "" and result.stderr or table.concat(output)), "\n")
        vim.notify("heucristic: build failed\n" .. table.concat(vim.list_slice(lines, math.max(1, #lines - 10)), "\n"), vim.log.levels.ERROR)
        return
      end
      local after = installed()
      if after and after ~= before then
        vim.notify("heucristic: built " .. after:sub(1, 7) .. ", restarting")
        restart()
      end
    end)
  end)
end

return M
