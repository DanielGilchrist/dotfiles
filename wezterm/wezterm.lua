require("commands").register_commands()

local os = require("os")
local wezterm = require("wezterm")
local mux = wezterm.mux
local os_utils = require("utils.os")
local key_utils = require("utils.key")
local table_utils = require("utils.table")

local command = key_utils.command_key()

local function set_path()
  local original_path = os.getenv("PATH")
  local system = os_utils.system()

  if system == "macos" then
    return "/opt/homebrew/bin:" .. original_path
  else
    return original_path
  end
end

wezterm.on("gui-startup", function(cmd)
  local _tab, _pane, window = mux.spawn_window(cmd or {})
  local gui_window = window:gui_window()

  gui_window:toggle_fullscreen()
end)

local misc = {
  set_environment_variables = {
    PATH = set_path()
  },
  front_end = "WebGpu",
  webgpu_power_preference = "HighPerformance",
  max_fps = 144,

  color_scheme = "Catppuccin Mocha",
  colors = {
    background = "#1E2528",
    foreground = "#F8F9E8",
    scrollbar_thumb = "#58686D",
    split = "#CBE3B3",

    tab_bar = {
      background = "#191E21",
      active_tab = {
        bg_color = "#CBE3B3",
        fg_color = "#171C1F",
      },
      new_tab = {
        bg_color = "#191E21",
        fg_color = "#CBE3B3",
      },
      new_tab_hover = {
        bg_color = "#262F33",
        fg_color = "#CBE3B3",
      },
      inactive_tab = {
        bg_color = "#191E21",
        fg_color = "#6F8788",
      },
      inactive_tab_hover = {
        bg_color = "#262F33",
        fg_color = "#ADC9BC",
      },
    },
  },

  automatically_reload_config = false,
  window_close_confirmation = "AlwaysPrompt",
  notification_handling = "AlwaysShow",
  exit_behavior = "Close",

  enable_scroll_bar = true, -- per pane scrollbar 👀  - https://github.com/wez/wezterm/pull/1886
  scrollback_lines = 50000,

  window_padding = {
    left = 0,
    right = 10, -- controls the width of the scrollbar
    top = 10,
    bottom = 0,
  },
  native_macos_fullscreen_mode = true,
  -- window_background_opacity = 0.90,
  -- macos_window_background_blur = 20,

  font = wezterm.font_with_fallback({
    {
      family = "JetBrainsMono NF",
      harfbuzz_features = { "liga=1" },
    },
  }),
  font_size = os_utils.system() == "macos" and 14 or 12,

  mouse_bindings = {
    -- CMD + click links
    {
      event = { Up = { streak = 1, button = "Left" } },
      mods = command,
      action = wezterm.action.OpenLinkAtMouseCursor,
    },
  },
}

local new_tab_icon = " + "
local tabs = {
  enable_tab_bar = true,
  use_fancy_tab_bar = false,
  hide_tab_bar_if_only_one_tab = true,

  tab_bar_style = {
    new_tab = new_tab_icon,
    new_tab_hover = new_tab_icon,
  },

  tab_max_width = 25,
}

return table_utils.merge_all(misc, require("key_bindings"), tabs, {})
