local wezterm = require("wezterm")

---@class TabUtils
---@field id fun(tab: any): integer|nil
---@field title fun(tab: any): string
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

-- Both tab objects are userdata that raise on unknown fields, so probe the
-- method and the field under pcall rather than indexing them directly.
---@param tab any
---@param key string
---@return any
local function field(tab, key)
  local ok, value = pcall(function() return tab[key] end)
  if ok then return value end
  return nil
end

---Title for either object: MuxTab has `:get_title()`, TabInformation has
---`.tab_title`.
---@param tab any
---@return string
function M.title(tab)
  if tab == nil then return "" end
  local get_title = field(tab, "get_title")
  if type(get_title) == "function" then return get_title(tab) or "" end
  return field(tab, "tab_title") or ""
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
