local notify = require("../utils/notify")
local notify_gem_list_id = "bundle-open-gem-list-id"

local function fetch_gem_list(callback)
  local gems = {}
  local error_message = nil

  notify.info("Fetching gem list for " .. vim.fn.getcwd() .. "...", {
    id = notify_gem_list_id,
    timeout = false,
  })

  local function on_stdout(_, data)
    for _, line in ipairs(data) do
      if line ~= "" then
        table.insert(gems, line)
      end
    end
  end

  local function on_stderr(_, data)
    error_message = table.concat(data)
  end

  local function on_exit()
    notify.hide(notify_gem_list_id)

    if vim.tbl_isempty(gems) then
      if error_message then
        notify.error(error_message)
      else
        notify.error("Unknown error. Likely an issue with `bundle` command.")
      end

      return
    end

    callback(gems)
  end

  vim.fn.jobstart({ "bundle", "list", "--name-only" }, {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = on_stdout,
    on_stderr = on_stderr,
    on_exit = on_exit
  })
end

local function open_gem()
  local function open_selected_gem(picker, item)
    picker:close()

    if item then
      local cwd = vim.fn.getcwd()
      local cmd = string.format(
        "wezterm cli spawn --cwd=%s -- bundle open %s",
        vim.fn.shellescape(cwd),
        vim.fn.shellescape(item.text)
      )
      vim.fn.system(cmd)
    end
  end

  fetch_gem_list(function(gems)
    local items = vim.tbl_map(function(gem)
      return {
        text = gem,
      }
    end, gems)

    Snacks.picker.pick({
      items = items,
      format = "text",
      preview = "none",
      source = "Bundle Open",
      confirm = open_selected_gem,
      layout = {
        preset = "vscode"
      }
    })
  end)
end

vim.api.nvim_create_user_command("BundleOpen", open_gem, {})
