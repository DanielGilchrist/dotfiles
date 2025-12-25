local cmd = require("utils.cmd")
local notify = require("utils.notify")
local is = require("utils.is")
local str = require("utils.str")

local function default_options(opts)
  opts = opts == nil and {} or opts

  return vim.tbl_extend(
    "force",
    {
      on_stdout = cmd.default_handler(notify.info),
      on_stderr = cmd.default_handler(notify.error),
    },
    opts
  )
end

local function arduino_cli(command, args, opts)
  args = args == nil and {} or args
  local combined_args = vim.list_extend({ command }, args)

  cmd.arduino_cli(combined_args, default_options(opts))
end

local function parse_board_list_line(line)
  local port, _protocol, _board_type, rest = line:match("^(%S+)%s+(%S+)%s+(%S+)%s+(.*)$")
  if not port or not rest then
    return nil
  end

  local board_name, fqbn = rest:match("^(.-)%s+(%S+:%S+:%S+)")
  if not board_name or not fqbn then
    return nil
  end

  return {
    port = port,
    fqbn = fqbn,
    name = str.trim(board_name),
  }
end

local function parse_boards(data)
  local boards = {}

  for i, line in ipairs(data) do
    -- First line is header
    if i > 1 and is.not_empty(line) then
      local board = parse_board_list_line(line)

      if board then
        table.insert(boards, board)
      end
    end
  end

  return boards
end

local function fetch_board_list(callback)
  local notify_id = "arduino-board-list-id"

  notify.info("Fetching connected boards...", {
    id = notify_id,
    timeout = false,
  })

  local function on_stdout(_, data)
    if is.empty(data) or #data == 1 then
      return
    end

    local boards = parse_boards(data)

    notify.hide(notify_id)

    if is.empty(boards) then
      return notify.warn("There are no boards connected!")
    end

    callback(boards)
  end

  arduino_cli("board", { "list" }, { on_stdout = on_stdout })
end

local function compile(args, opts)
  arduino_cli("compile", args, opts)
end

local function upload(args)
  arduino_cli("upload", args)
end

local function run(board)
  local compile_opts = default_options({
    on_stdout = function(_, _data)
      notify.info("Compile successful, uploading...")

      local args = {}

      if board then
        vim.list_extend(args, { "-p", board.port, "-b", board.fqbn })
      end

      upload(args)
    end
  })

  local args = {}

  if board then
    vim.list_extend(args, { "-b", board.fqbn })
  end


  compile(args, compile_opts)
end

local function run_select()
  local function on_select(picker, item)
    picker:close()

    if item then
      run(item.board)
    end
  end

  fetch_board_list(function(boards)
    local items = vim.tbl_map(function(board)
      return {
        text = board.name,
        board = board,
      }
    end, boards)

    Snacks.picker.pick({
      items = items,
      format = "text",
      preview = "none",
      source = "Arduino Boards",
      confirm = on_select,
      layout = {
        preset = "vscode"
      }
    })
  end)
end

vim.api.nvim_create_user_command(
  "ArduinoCompile",
  function()
    compile()
  end,
  { desc = "Compile for configured board in local sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoUpload",
  function()
    upload()
  end,
  { desc = "Upload to configured board in local sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoRun",
  function()
    run()
  end,
  { desc = "Compile and upload to configured board in sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoRunSelect",
  run_select,
  { desc = "Select board then compile and upload to it" }
)

vim.keymap.set("n", "<leader>rc", "<cmd>ArduinoCompile<cr>", { desc = "Arduino: Compile" })
vim.keymap.set("n", "<leader>ru", "<cmd>ArduinoUpload<cr>", { desc = "Arduino: Upload" })
vim.keymap.set("n", "<leader>rr", "<cmd>ArduinoRun<cr>", { desc = "Arduino: Run (Compile + Upload)" })
vim.keymap.set("n", "<leader>rs", "<cmd>ArduinoRunSelect<cr>", { desc = "Arduino: Select Board and Run" })
