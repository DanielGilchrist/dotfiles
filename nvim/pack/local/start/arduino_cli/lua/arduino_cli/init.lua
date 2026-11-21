local cmd = require("utils.cmd")
local notify = require("utils.notify")
local is = require("utils.is")

---@param opts? CliOpts
---@return CliOpts
local function default_options(opts)
  opts = opts or {}

  return vim.tbl_extend("force", {
    on_stdout = cmd.default_handler(notify.info),
    on_stderr = cmd.default_handler(notify.error)
  }, opts
  )
end

---@param command string
---@param args?   string[]
---@param opts?   CliOpts
local function arduino_cli(command, args, opts)
  args = args or {}
  local combined_args = vim.list_extend({ command }, args)

  cmd.arduino_cli(combined_args, default_options(opts))
end

---@class ArduinoBoard
---@field port string
---@field fqbn string
---@field name string

---@param line string
---@return ArduinoBoard?
local function parse_board_list_line(line)
  local port, _protocol, _board_type, rest = line:match("^(%S+)%s+(%S+)%s+(%S+)%s+(.*)$")
  if not port or not rest then
    return nil
  end

  local board_name, fqbn = rest:match("^(.-)%s+(%S+:%S+:%S+)")
  if not board_name or not fqbn then
    return nil
  end

  return { port = port, fqbn = fqbn, name = vim.trim(board_name) }
end

---@param data string[]
---@return ArduinoBoard[]
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

---@param callback fun(boards: ArduinoBoard[])
local function fetch_board_list(callback)
  local notify_id = "arduino-board-list-id"

  notify.info("Fetching connected boards...", {
    id = notify_id,
    timeout = false
  })

  ---@param _    integer
  ---@param data string[]
  local function on_stdout(_, data)
    if is.empty(data) or #data == 1 then
      return
    end

    local boards = parse_boards(data)

    notify.hide(notify_id)

    if is.empty(boards) then
      notify.warn("There are no boards connected!")
      return
    end

    callback(boards)
  end

  arduino_cli("board", { "list" }, { on_stdout = on_stdout })
end

---@param args? string[]
---@param opts? CliOpts
local function compile(args, opts)
  arduino_cli("compile", args, opts)
end

---@param args? string[]
local function upload(args)
  arduino_cli("upload", args)
end

---@param board? ArduinoBoard
local function run(board)
  local compile_opts = default_options({
    on_stdout = function (_, _data)
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
  ---@param picker snacks.Picker
  ---@param item   snacks.picker.Item?
  local function on_select(picker, item)
    picker:close()

    if item then
      run(item.board)
    end
  end

  fetch_board_list(function (boards)
    local items = vim.tbl_map(function (board)
      return { text = board.name, board = board }
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

return { compile = compile, upload = upload, run = run, run_select = run_select }
