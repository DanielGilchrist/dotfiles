local M = {}

local repo = vim.fn.expand("~/repos/DanielGilchrist/heucristic")
local building = false

local function head()
  local result = vim.system({ "git", "-C", repo, "rev-parse", "HEAD" }, { text = true }):wait()
  if result.code ~= 0 then
    return nil
  end
  return vim.trim(result.stdout)
end

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
  local current = head()
  if not current or (current == installed() and vim.fn.executable(repo .. "/bin/heucristic") == 1) then
    return
  end

  building = true
  vim.notify("heucristic: building " .. current:sub(1, 7) .. " in the background")
  vim.system({ "script/install" }, { cwd = repo, text = true }, function(result)
    vim.schedule(function()
      building = false
      if result.code ~= 0 then
        local lines = vim.split(vim.trim(result.stderr ~= "" and result.stderr or result.stdout), "\n")
        vim.notify("heucristic: build failed\n" .. table.concat(vim.list_slice(lines, math.max(1, #lines - 10)), "\n"), vim.log.levels.ERROR)
        return
      end
      vim.notify("heucristic: built " .. current:sub(1, 7) .. ", restarting")
      restart()
    end)
  end)
end

return M
