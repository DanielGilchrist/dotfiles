local wezterm = require "wezterm"
local os_utils = require "utils.os"

local commands = {}

local function split_pane_with(func)
  return function(pane, direction, size)
    size = size == nil and 0.5 or size
    local new_pane = pane:split { direction = direction, size = size }
    func(new_pane)
    return new_pane
  end
end

local function has_text(pane, ...)
  local text = pane:get_logical_lines_as_text(pane:get_dimensions().scrollback_rows)

  for i = 1, select("#", ...) do
    local pattern = select(i, ...)

    if string.find(text, pattern) then
      return true
    end
  end

  return false
end

local function run_command(pane, command)
  pane:send_text(command .. "\n")
end

local function wait_for_text_for(pane, ...)
  while true do
    if has_text(pane, ...) then
      break
    else
      wezterm.sleep_ms(3000)
    end
  end
end

local function notify(window, title, content)
  -- idk why but I can't get `window:toast_notification` to work on mac for the life of me
  if os_utils.system() == "macos" then
    local q = "\""
    local command = "display notification " .. q .. content .. q .. " with title " .. q .. title .. q
    wezterm.run_child_process { "osascript", "-e", command }
  else
    window:toast_notification(title, content, nil, 4000)
  end
end

local function clear(pane)
  pane:send_text('\x0c')
end

local function has_interrupted_system_call(pane)
  return has_text(pane, "cd: Interrupted system call")
end

local function buffered_cdt(pane)
  pane:send_text("cdt")
  wezterm.sleep_ms(100)
  pane:send_text("\n")
end

local function wait_for_text()
  wezterm.sleep_ms(100)
end

local function handle_potential_interrupted_system_call_from_cdt(pane, already_tried)
  if has_interrupted_system_call(pane) then
    if already_tried then
      notify(pane.window, "OpenWorkTabs", "Error! Can't continue due to issue with executing `cdt`")
      return true
    else
      clear(pane)
      buffered_cdt(pane)
      wait_for_text()
      return handle_potential_interrupted_system_call_from_cdt(pane, true)
    end
  else
    return
  end
end

local function handle_missing_cdt_command(pane)
  if has_text(pane, "Unknown command") then
    notify(pane.window, "OpenWorkTabs", "Error! Please set a `cdt` alias")
    return true
  end
end

local function open_work_tabs(region)
  return function(original_window, _original_pane, _line)
    local function export_region_and_cdt(pane)
      run_command(pane, "export REGION=" .. region)
      run_command(pane, "cdt")
    end

    local split_pane_with_setup = split_pane_with(function(new_pane)
      export_region_and_cdt(new_pane)
    end)

    local _tab, server_pane, window = original_window:mux_window():spawn_tab {}

    export_region_and_cdt(server_pane)
    wait_for_text()

    if handle_missing_cdt_command(server_pane) then
      return
    end

    if handle_potential_interrupted_system_call_from_cdt(server_pane) then
      return
    end

    local console_pane = split_pane_with_setup(server_pane, "Right")
    local webpack_pane = split_pane_with_setup(console_pane, "Bottom", 0.1)
    local tunnel_pane = split_pane_with_setup(server_pane, "Bottom", 0.1)
    local worker_pane = split_pane_with_setup(server_pane, "Bottom", 0.4)

    run_command(tunnel_pane, "bin/tunnel")
    wait_for_text_for(tunnel_pane, "INFO: Ready!")

    run_command(server_pane, "bin/dev server")
    wait_for_text_for(server_pane, "Connection to", " closed")

    run_command(webpack_pane, "bin/dev webpack")
    run_command(console_pane, "bin/dev console")
    run_command(worker_pane, "bin/dev worker")
  end
end

commands.register_commands = function()
  wezterm.on("augment-command-palette", function(_window, _pane)
    return {
      {
        brief = "[APAC] Open work tabs",
        action = wezterm.action_callback(open_work_tabs("apac"))
      },
      {
        brief = "[EU] Open work tabs",
        action = wezterm.action_callback(open_work_tabs("eu"))
      }
    }
  end)
end

return commands
