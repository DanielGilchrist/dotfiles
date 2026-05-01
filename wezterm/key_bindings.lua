local os = require("os")
local io = require("io")

local wezterm = require("wezterm")
local key_utils = require("utils.key")
local os_utils = require("utils.os")
local agents_tab = require("agents_tab")
local worktree_picker = require("worktree_picker")

---@param mods string
---@param key string
---@param action Action
---@return Key
local function keybind(mods, key, action)
  return { mods = mods, key = key, action = action }
end

---@param key1 string
---@param key2 string
---@return string
local function combine(key1, key2)
  return key1 .. "|" .. key2
end

---@return string
local function set_path()
  local original_path = os.getenv("PATH") or ""
  local system = os_utils.system()

  if system == "macos" then
    return "/opt/homebrew/bin:" .. original_path
  else
    return original_path
  end
end

---@class KeyConfig
---@field keys Key[]
---@field key_tables table<string, Key[]>
local config = {}

---@class KeyMods
---@field COMMAND string
---@field SHIFT string
---@field ALT string
---@field CTRL string
---@field ENTER string
---@field COMMAND_SHIFT string
---@field COMMAND_ALT string
---@field SHIFT_ALT string
local keys = {
  COMMAND = key_utils.command_key(),
  SHIFT = "SHIFT",
  ALT = "ALT",
  CTRL = "CTRL",
  ENTER = "Enter",
}

keys.COMMAND_SHIFT = combine(keys.COMMAND, keys.SHIFT)
keys.COMMAND_ALT = combine(keys.COMMAND, keys.ALT)
keys.SHIFT_ALT = combine(keys.SHIFT, keys.ALT)

config.keys = {
  keybind(keys.COMMAND_SHIFT, "r", "ReloadConfiguration"),

  keybind(keys.COMMAND, "t", wezterm.action.SpawnTab("CurrentPaneDomain")),
  keybind(keys.COMMAND_SHIFT, "n", wezterm.action.SpawnWindow),

  keybind(keys.COMMAND_SHIFT, "f", wezterm.action_callback(function(window, pane)
    local text = pane:get_lines_as_text(pane:get_dimensions().scrollback_rows)

    local name = os.tmpname()
    local file = io.open(name, "w+")
    ---@cast file -nil

    file:write(text)
    file:flush()
    file:close()

    window:perform_action(
      wezterm.action.SpawnCommandInNewTab({
        args = { "nvim", name },
        set_environment_variables = {
          PATH = set_path(),
        },
      }),
      pane
    )

    wezterm.sleep_ms(1000)
    os.remove(name)
  end)),

  keybind(keys.COMMAND, "d", wezterm.action({ SplitHorizontal = { domain = "CurrentPaneDomain" } })),
  keybind(keys.COMMAND, "s", wezterm.action({ SplitPane = { direction = "Right", size = { Percent = 35 } } })),
  keybind(keys.COMMAND_SHIFT, "d", wezterm.action({ SplitVertical = { domain = "CurrentPaneDomain" } })),

  keybind(keys.COMMAND, "k", wezterm.action_callback(function(window, pane)
    local process_name = pane:get_foreground_process_name()
    if process_name and process_name:find("nvim") then
      return
    end

    window:perform_action(wezterm.action({ ClearScrollback = "ScrollbackAndViewport" }), pane)
  end)),

  -- CMD+W / CMD+Shift+W refuse to act on the agents tab. Use `agent-rm` to remove.
  keybind(keys.COMMAND, "w", agents_tab.close_pane),
  keybind(keys.COMMAND_SHIFT, "w", agents_tab.close_tab),

  keybind(keys.COMMAND, keys.ENTER, "ToggleFullScreen"),
  keybind(keys.SHIFT, keys.ENTER, wezterm.action({ SendString = "\x1b[13;2u" })),

  -- Pane navigation: hjkl + arrows (CMD+Alt)
  keybind(keys.COMMAND_ALT, "h", wezterm.action({ ActivatePaneDirection = "Left" })),
  keybind(keys.COMMAND_ALT, "l", wezterm.action({ ActivatePaneDirection = "Right" })),
  keybind(keys.COMMAND_ALT, "k", wezterm.action({ ActivatePaneDirection = "Up" })),
  keybind(keys.COMMAND_ALT, "j", wezterm.action({ ActivatePaneDirection = "Down" })),

  keybind(keys.COMMAND_ALT, "LeftArrow", wezterm.action({ ActivatePaneDirection = "Left" })),
  keybind(keys.COMMAND_ALT, "RightArrow", wezterm.action({ ActivatePaneDirection = "Right" })),
  keybind(keys.COMMAND_ALT, "UpArrow", wezterm.action({ ActivatePaneDirection = "Up" })),
  keybind(keys.COMMAND_ALT, "DownArrow", wezterm.action({ ActivatePaneDirection = "Down" })),

  -- Switch between tabs (skips the agents tab when cycling)
  keybind(keys.COMMAND, "LeftArrow", agents_tab.cycle_left),
  keybind(keys.COMMAND, "RightArrow", agents_tab.cycle_right),

  -- Scrolling
  keybind(keys.COMMAND_SHIFT, "UpArrow", "ScrollToTop"),
  keybind(keys.COMMAND_SHIFT, "DownArrow", "ScrollToBottom"),
  keybind(keys.ALT, "PageUp", wezterm.action({ ScrollByPage = -1 })),
  keybind(keys.ALT, "PageDown", wezterm.action({ ScrollByPage = 1 })),

  -- Move tabs (refuses to move agents tab; skips it as a swap target)
  keybind(keys.SHIFT_ALT, "{", agents_tab.move_left),
  keybind(keys.SHIFT_ALT, "}", agents_tab.move_right),

  -- Pane swap, select, zoom
  keybind(keys.COMMAND_SHIFT, "m", wezterm.action({ PaneSelect = { mode = "SwapWithActive" } })),
  keybind(keys.COMMAND_SHIFT, "p", wezterm.action({ PaneSelect = { mode = "Activate" } })),
  keybind(keys.COMMAND_SHIFT, "z", wezterm.action.TogglePaneZoomState),

  -- Resize key table (CMD+r enters; spam hjkl; Esc exits)
  keybind(keys.COMMAND, "r", wezterm.action.ActivateKeyTable({
    name = "resize",
    one_shot = false,
    timeout_milliseconds = 2000,
    until_unknown = true,
  })),

  -- Agents tab toggle (jump to agents / back to last tab)
  keybind(keys.COMMAND, "0", agents_tab.toggle),
  -- Pin the agents tab to position 0
  keybind(keys.COMMAND_SHIFT, "0", agents_tab.pin_to_zero),

  -- Worktree picker (jumps to a worktree dir in a new tab)
  keybind(keys.COMMAND_SHIFT, "o", wezterm.action_callback(function(window, pane)
    worktree_picker.open(window, pane)
  end)),
}

config.key_tables = {
  resize = {
    keybind("NONE", "h", wezterm.action({ AdjustPaneSize = { "Left", 5 } })),
    keybind("NONE", "j", wezterm.action({ AdjustPaneSize = { "Down", 5 } })),
    keybind("NONE", "k", wezterm.action({ AdjustPaneSize = { "Up", 5 } })),
    keybind("NONE", "l", wezterm.action({ AdjustPaneSize = { "Right", 5 } })),
    keybind("NONE", "Escape", "PopKeyTable"),
    keybind("NONE", "Enter", "PopKeyTable"),
    keybind(keys.CTRL, "c", "PopKeyTable"),
  },
}

return config
