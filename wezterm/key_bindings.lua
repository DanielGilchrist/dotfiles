local os = require("os")
local io = require("io")

local wezterm = require("wezterm")
local key_utils = require("utils.key")
local os_utils = require("utils.os")

local function keybind(mods, key, action)
  return { mods = mods, key = key, action = action }
end

local function combine(key1, key2)
  return key1 .. "|" .. key2
end

local function set_path()
  local original_path = os.getenv("PATH")
  local system = os_utils.system()

  if system == "macos" then
    return "/opt/homebrew/bin:" .. original_path
  else
    return original_path
  end
end

local config = {}
local keys = {
  COMMAND = key_utils.command_key(),
  SHIFT = "SHIFT",
  ALT = "ALT",
  ENTER = "Enter",
}

keys.COMMAND_SHIFT = combine(keys.COMMAND, keys.SHIFT)
keys.COMMAND_ALT = combine(keys.COMMAND, keys.ALT)
keys.SHIFT_ALT = combine(keys.SHIFT, keys.ALT)

config.keys = {
  keybind(keys.COMMAND_SHIFT, "r", "ReloadConfiguration"),

  keybind(keys.COMMAND_SHIFT, "n", wezterm.action.SpawnWindow),

  keybind(keys.COMMAND_SHIFT, "f", wezterm.action_callback(function(window, pane)
    local text = pane:get_lines_as_text(pane:get_dimensions().scrollback_rows)

    local name = os.tmpname()
    local file = io.open(name, 'w+')
    ---@cast file -nil

    file:write(text)
    file:flush()
    file:close()

    window:perform_action(
      wezterm.action.SpawnCommandInNewTab {
        args = { "nvim", name },
        set_environment_variables = {
          PATH = set_path()
        }
      },
      pane
    )

    -- Wait a likely enough amount of time for neovim to have read the file before we remove it.
    wezterm.sleep_ms(1000)
    os.remove(name)
  end)),

  keybind(
    keys.COMMAND,
    "d",
    wezterm.action({
      SplitHorizontal = {
        domain = "CurrentPaneDomain",
      },
    })
  ),
  keybind(
    keys.COMMAND,
    "s",
    wezterm.action({
      SplitPane = {
        direction = "Right",
        size = { Percent = 35 },
      },
    })
  ),

  keybind(
    keys.COMMAND_SHIFT,
    "d",
    wezterm.action({
      SplitVertical = {
        domain = "CurrentPaneDomain",
      },
    })
  ),

  keybind(keys.COMMAND, "k", wezterm.action({ ClearScrollback = "ScrollbackAndViewport" })),

  keybind(
    keys.COMMAND,
    "w",
    wezterm.action({
      CloseCurrentPane = {
        confirm = false,
      },
    })
  ),

  keybind(
    keys.COMMAND_SHIFT,
    "w",
    wezterm.action({
      CloseCurrentTab = {
        confirm = false,
      },
    })
  ),

  keybind(keys.COMMAND, keys.ENTER, "ToggleFullScreen"),

  keybind(keys.SHIFT, keys.ENTER, wezterm.action({ SendString = "\x1b[13;2u" })),

  -- Switch active pane
  keybind(keys.COMMAND_ALT, "LeftArrow", wezterm.action({ ActivatePaneDirection = "Left" })),
  keybind(keys.COMMAND_ALT, "RightArrow", wezterm.action({ ActivatePaneDirection = "Right" })),
  keybind(keys.COMMAND_ALT, "UpArrow", wezterm.action({ ActivatePaneDirection = "Up" })),
  keybind(keys.COMMAND_ALT, "DownArrow", wezterm.action({ ActivatePaneDirection = "Down" })),

  -- Switch between tabs
  keybind(keys.COMMAND, "LeftArrow", wezterm.action({ ActivateTabRelative = -1 })),
  keybind(keys.COMMAND, "RightArrow", wezterm.action({ ActivateTabRelative = 1 })),

  -- Scrolling
  keybind(keys.COMMAND_SHIFT, "UpArrow", "ScrollToTop"),
  keybind(keys.COMMAND_SHIFT, "DownArrow", "ScrollToBottom"),
  keybind(keys.ALT, "PageUp", wezterm.action({ ScrollByPage = -1 })),
  keybind(keys.ALT, "PageDown", wezterm.action({ ScrollByPage = 1 })),
  keybind(keys.COMMAND_ALT, "PageUp", wezterm.action({ ScrollByPage = -6 })),
  keybind(keys.COMMAND_ALT, "PageDown", wezterm.action({ ScrollByPage = 6 })),

  -- Move tabs
  keybind(keys.SHIFT_ALT, "{", wezterm.action({ MoveTabRelative = -1 })),
  keybind(keys.SHIFT_ALT, "}", wezterm.action({ MoveTabRelative = 1 })),

  -- Move panes
  keybind(keys.COMMAND_SHIFT, "m", wezterm.action({ PaneSelect = { mode = "SwapWithActive" } })),
  keybind(keys.COMMAND_SHIFT, "p", wezterm.action({ PaneSelect = { mode = "Activate" } })),

  -- Resize panes
  keybind(keys.COMMAND_ALT, "h", wezterm.action({ AdjustPaneSize = { "Left", 5 } })),
  keybind(keys.COMMAND_ALT, "l", wezterm.action({ AdjustPaneSize = { "Right", 5 } })),
  keybind(keys.COMMAND_ALT, "k", wezterm.action({ AdjustPaneSize = { "Up", 5 } })),
  keybind(keys.COMMAND_ALT, "j", wezterm.action({ AdjustPaneSize = { "Down", 5 } })),
}

return config
