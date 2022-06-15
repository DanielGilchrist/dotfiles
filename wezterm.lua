local wezterm = require 'wezterm';
local os_utils = require 'util.os'.os_utils
local table_utils = require 'util.table'.table_utils

local misc = {
  native_macos_fullscreen_mode = true,
  automatically_reload_config = false,
  window_close_confirmation = "NeverPrompt",
  exit_behavior = "Close",
  window_padding = {
    left = 2,
    right = 2,
    top = 2,
    bottom = 2,
  },
  color_scheme = "MaterialOcean",
  font = wezterm.font_with_fallback({
    {
      family = "JetBrains Mono",
      harfbuzz_features = { "liga=1" },
    },
    {
      family = "MesloLGS NF",
    }
  }),
  font_size = os_utils.system() == "macos" and 14 or 12,
  initial_cols = 150,
  initial_rows = 50,
}

return table_utils.merge_all(
  misc,
  require("key_bindings"),
  {}
)
