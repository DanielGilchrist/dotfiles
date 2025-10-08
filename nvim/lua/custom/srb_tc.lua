local notify = require("../utils/notify")
local notify_id = "srb-tc-id"

local function parse_srb_errors(output)
  local files = {}
  local seen = {}

  for line in output:gmatch("[^\r\n]+") do
    local file, line_num = line:match("^([^:]+):(%d+):")
    if file and line_num and not seen[file] then
      table.insert(files, { file = file, line = line_num })
      seen[file] = true
    end
  end

  return files
end

local function open_files_in_new_tab(files)
  if #files == 0 then
    notify.info("No type errors found!")
    return
  end

  local cwd = vim.fn.getcwd()
  local file_args = {}

  for _, item in ipairs(files) do
    table.insert(file_args, item.file)
  end

  local cmd =
    string.format("wezterm cli spawn --cwd=%s -- nvim %s", vim.fn.shellescape(cwd), table.concat(file_args, " "))

  vim.fn.system(cmd)
  notify.info(string.format("Opened %d files in new tab", #files))
end

local function run_srb_tc()
  notify.info("Running bundle exec srb tc...", {
    id = notify_id,
    timeout = false,
  })

  local output = ""
  local error_output = ""

  local function on_stdout(_, data)
    output = output .. table.concat(data, "\n")
  end

  local function on_stderr(_, data)
    error_output = error_output .. table.concat(data, "\n")
  end

  local function on_exit(_, code)
    notify.hide(notify_id)

    if code == 0 then
      notify.info("No type errors found!")
      return
    end

    local combined_output = output .. error_output
    local files = parse_srb_errors(combined_output)

    if #files > 0 then
      open_files_in_new_tab(files)
    else
      notify.error("Failed to parse srb tc output")
    end
  end

  vim.fn.jobstart({ "bundle", "exec", "srb", "tc" }, {
    stdout_buffered = true,
    stderr_buffered = true,
    on_stdout = on_stdout,
    on_stderr = on_stderr,
    on_exit = on_exit,
  })
end

vim.api.nvim_create_user_command("SrbTc", run_srb_tc, {})
