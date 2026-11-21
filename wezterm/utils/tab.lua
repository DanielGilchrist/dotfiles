local wezterm = require("wezterm")

---@class TabUtils
---@field id                     fun(tab: MuxTab | TabInformation | nil): integer | nil
---@field title                  fun(tab: MuxTab | TabInformation | nil): string
---@field first_non_pinned_index fun(mux_window: MuxWindow): integer
---@field move_to_first          fun(gui_window: Window, pane: Pane): nil
local M = {}

-- Both tab objects are userdata that raise on unknown fields, so probe for
-- MuxTab's method under pcall rather than indexing it directly.
---@param tab MuxTab | TabInformation
---@return boolean
---@return_cast tab MuxTab else TabInformation
local function is_mux_tab(tab)
  local ok, get_title = pcall(function () return tab.get_title end)
  return ok and type(get_title) == "function"
end

--- Tab id for either object wezterm hands out: MuxTab has `:tab_id()`,
--- TabInformation (format-tab-title etc.) has a plain `.tab_id` number.
---@param tab MuxTab | TabInformation | nil
---@return integer | nil
function M.id(tab)
  if tab == nil then return nil end
  if is_mux_tab(tab) then return tab:tab_id() end
  return tab.tab_id
end

--- Title for either object: MuxTab has `:get_title()`, TabInformation has
--- `.tab_title`.
---@param tab MuxTab | TabInformation | nil
---@return string
function M.title(tab)
  if tab == nil then return "" end
  if is_mux_tab(tab) then return tab:get_title() end
  return tab.tab_title
end

--- Find the leftmost index that's safe to move a freshly-spawned tab into,
--- i.e. skipping any pinned tabs (currently just the agents tab when it sits at 0).
---@param mux_window MuxWindow
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

--- Move the currently-active tab to the leftmost non-pinned position.
---@param gui_window Window
---@param pane       Pane
function M.move_to_first(gui_window, pane)
  local target = M.first_non_pinned_index(gui_window:mux_window())
  gui_window:perform_action(wezterm.action.MoveTab(target), pane)
end

return M
