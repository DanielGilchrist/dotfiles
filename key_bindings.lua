local wezterm = require 'wezterm'

local function keybind(mods, key, action)
  return {mods = mods, key = key, action = action}
end

local function combine(key1, key2)
  return key1 .. '|' .. key2
end

local config = {}

local command = "CMD"
local shift = "SHIFT"
local alt = "ALT"
local command_shift = combine(command, shift)
local command_alt = combine(command, alt)

config.keys = {
  keybind(command_shift, "r", "ReloadConfiguration"),

  keybind(command, "d", wezterm.action{SplitHorizontal={
    domain="CurrentPaneDomain",
  }}),

  keybind(command_shift, "d", wezterm.action{SplitVertical={
    domain="CurrentPaneDomain",
  }}),

  keybind(command, "k", wezterm.action{ClearScrollback="ScrollbackAndViewport"}),

  keybind(command, "w", wezterm.action{
    CloseCurrentPane={
      confirm = false,
    }
  }),

  keybind(command_shift, "w", wezterm.action{
    CloseCurrentTab={
      confirm = true,
    }
  }),

  keybind(command_alt, "LeftArrow", wezterm.action{ActivatePaneDirection="Left"}),
  keybind(command_alt, "RightArrow", wezterm.action{ActivatePaneDirection="Right"}),
  keybind(command_alt, "UpArrow", wezterm.action{ActivatePaneDirection="Up"}),
  keybind(command_alt, "DownArrow", wezterm.action{ActivatePaneDirection="Down"}),

  keybind(command, "LeftArrow", wezterm.action{ActivateTabRelative=-1}),
  keybind(command, "RightArrow", wezterm.action{ActivateTabRelative=1}),

  keybind(command_shift, "UpArrow", "ScrollToTop"),
  keybind(command_shift, "DownArrow", "ScrollToBottom"),

  keybind("CMD", "Enter", "ToggleFullScreen")
}

return config
