local wezterm = require("wezterm")
local notify = require("utils.notify")

---@class DevTabsModule
---@field is_dev fun(tab: MuxTab|TabInformation|nil): boolean
---@field toggle Action
local M = {}

local REGIONS = { "apac", "eu", "us" }

---Dev tabs are tracked per-region in wezterm.GLOBAL.dev_tab_id_by_region
---(populated by commands.spawn_dev_tab).
function M.is_dev(tab)
  if tab == nil then return false end

  -- wezterm.GLOBAL wraps stored tables in a userdata proxy, so no type()
  -- check, and index by known region keys rather than trusting pairs() to
  -- iterate the proxy.
  local by_region = wezterm.GLOBAL.dev_tab_id_by_region

  if by_region == nil then return false end

  -- MuxTab has :tab_id() but TabInformation has .tab_id (a number).
  local tid = type(tab.tab_id) == "function" and tab:tab_id() or tab.tab_id

  for _, region in ipairs(REGIONS) do
    if tid == by_region[region] then return true end
  end

  return false
end

M.toggle = wezterm.action_callback(function(window)
  local dev_tabs = {}

  for _, t in ipairs(window:mux_window():tabs()) do
    if M.is_dev(t) then table.insert(dev_tabs, t) end
  end

  if #dev_tabs == 0 then
    notify(window, "dev", "no dev tabs — CMD+Shift+R spawns one")
    return
  end

  local active = window:active_tab()
  if not active then return end

  local active_id = active:tab_id()
  local current

  for i, t in ipairs(dev_tabs) do
    if t:tab_id() == active_id then
      current = i
      break
    end
  end

  if not current then
    wezterm.GLOBAL.dev_prev_tab = active_id
    local target = dev_tabs[1]

    for _, t in ipairs(dev_tabs) do
      if t:tab_id() == wezterm.GLOBAL.dev_last_tab then
        target = t
        break
      end
    end

    wezterm.GLOBAL.dev_last_tab = target:tab_id()
    target:activate()

    return
  end

  local next_tab = dev_tabs[current + 1]
  if next_tab then
    wezterm.GLOBAL.dev_last_tab = next_tab:tab_id()
    next_tab:activate()

    return
  end

  local prev_id = wezterm.GLOBAL.dev_prev_tab

  for _, t in ipairs(window:mux_window():tabs()) do
    if t:tab_id() == prev_id then
      t:activate()
      return
    end
  end

  for _, t in ipairs(window:mux_window():tabs()) do
    if not M.is_dev(t) then
      t:activate()
      return
    end
  end
end)

return M
