local wezterm = require("wezterm")

---@class TabUtils
---@field id fun(tab: any): integer|nil
---@field first_non_pinned_index fun(mux_window: any): integer
---@field move_to_first fun(gui_window: any, pane: any): nil
local M = {}

---Tab id for either object wezterm hands out: MuxTab has `:tab_id()`,
---TabInformation (format-tab-title etc.) has a plain `.tab_id` number.
---@param tab any
---@return integer|nil
function M.id(tab)
  if tab == nil then return nil end
  if type(tab.tab_id) == "function" then return tab:tab_id() end
  return tab.tab_id
end

---Find the leftmost index that's safe to move a freshly-spawned tab into,
---i.e. skipping any pinned tabs (currently just the agents tab when it sits at 0).
---@param mux_window any
---@return integer 0-based index
function M.first_non_pinned_index(mux_window)
  -- Required lazily: agents_tab depends on this module for `id`.
  local agents_tab = require("agents_tab")
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
