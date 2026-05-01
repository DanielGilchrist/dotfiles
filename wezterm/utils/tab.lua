local wezterm = require("wezterm")
local agents_tab = require("agents_tab")

---@class TabUtils
---@field first_non_pinned_index fun(mux_window: any): integer
---@field move_to_first fun(gui_window: any, pane: any): nil
local M = {}

---Find the leftmost index that's safe to move a freshly-spawned tab into,
---i.e. skipping any pinned tabs (currently just the agents tab when it sits at 0).
---@param mux_window any
---@return integer 0-based index
function M.first_non_pinned_index(mux_window)
  local tabs = mux_window:tabs()
  if #tabs > 0 and tabs[1]:get_title() == agents_tab.TITLE then
    return 1
  end
  return 0
end

---Move the currently-active tab to the leftmost non-pinned position.
---@param gui_window any
---@param pane any
function M.move_to_first(gui_window, pane)
  local target = M.first_non_pinned_index(gui_window:mux_window())
  gui_window:perform_action(wezterm.action.MoveTab(target), pane)
end

return M
