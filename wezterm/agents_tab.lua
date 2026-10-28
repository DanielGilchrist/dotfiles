local wezterm = require("wezterm")
local tab_utils = require("utils.tab")
local dev_tabs = require("dev_tabs")

---@class AgentsTabModule
---@field TITLE string
---@field toggle Action
---@field cycle_left Action
---@field cycle_right Action
---@field move_left Action
---@field move_right Action
---@field split_horizontal Action
---@field split_vertical Action
---@field split_right_35 Action
---@field pin_to_zero Action
---@field register fun(): nil
local M = { TITLE = "agents" }

-- The agents tab runs the `agent` TUI. It is identified by its wezterm tab
-- id, kept in wezterm.GLOBAL, which survives config reloads but not a
-- wezterm restart, so fall back to the title and adopt the id.
local function is_agents(tab)
  if tab == nil then return false end
  local id = wezterm.GLOBAL.agents_tab_id
  local tid = tab_utils.id(tab)
  if type(id) == "number" and tid == id then return true end
  if tab_utils.title(tab) == M.TITLE then
    if type(id) ~= "number" then wezterm.GLOBAL.agents_tab_id = tid end
    return true
  end
  return false
end

local function find_agents(window)
  for _, t in ipairs(window:mux_window():tabs()) do
    if is_agents(t) then return t end
  end
  return nil
end

---The agents tab in any window, so a second window reuses the running TUI.
---@return MuxTab|nil
local function find_agents_anywhere(window)
  local here = find_agents(window)
  if here then return here end
  for _, mux_window in ipairs(wezterm.mux.all_windows()) do
    for _, t in ipairs(mux_window:tabs()) do
      if is_agents(t) then return t end
    end
  end
  wezterm.GLOBAL.agents_tab_id = nil
  return nil
end

-- macOS GUI apps don't inherit shell PATH; route through the user's login
-- shell so its profile (which sources Homebrew etc.) sets PATH up for the
-- TUI and the zellij, git and harness processes it runs.
local SHELL = os.getenv("SHELL") or "/bin/sh"

-- MoveTab acts on the active tab, so we activate-move-restore.
local function pin_to_zero(window, tab)
  local prev = window:active_tab()
  tab:activate()
  window:perform_action(wezterm.action.MoveTab(0), window:active_pane())
  if prev and prev:tab_id() ~= tab:tab_id() then prev:activate() end
end

local function spawn_agents_tab(window)
  local tab = window:mux_window():spawn_tab({
    args = { SHELL, "-l", "-c", "exec agent" },
  })
  if not tab then return nil end
  wezterm.GLOBAL.agents_tab_id = tab:tab_id()
  tab:set_title(M.TITLE)
  pin_to_zero(window, tab)
  return tab
end

---Activate the agents tab, starting the TUI if it isn't running.
---@param window Window
function M.focus(window)
  local agents = find_agents_anywhere(window) or spawn_agents_tab(window)
  if not agents then return end
  local active = window:active_tab()
  if active and active:tab_id() ~= agents:tab_id() then wezterm.GLOBAL.agent_prev_tab = active:tab_id() end
  agents:activate()
  local gui = agents:window():gui_window()
  if gui then gui:focus() end
end

-- Find + pin, with one retry for the spawn → mux-registration race.
local function pin_with_retry(window)
  local function try()
    local agents = find_agents(window)
    if agents then
      pin_to_zero(window, agents); return true
    end
    return false
  end
  if not try() then wezterm.time.call_after(0.3, try) end
end

M.pin_to_zero = wezterm.action_callback(pin_with_retry)

M.toggle = wezterm.action_callback(function(window)
  local agents = find_agents(window)
  local active = window:active_tab()
  if not agents or not active or active:tab_id() ~= agents:tab_id() then
    M.focus(window)
    return
  end

  -- Returning from agents: prefer remembered tab, else first non-agents.
  local prev_id = wezterm.GLOBAL.agent_prev_tab
  local fallback
  for _, t in ipairs(window:mux_window():tabs()) do
    if not is_agents(t) then
      if t:tab_id() == prev_id then
        t:activate(); return
      end
      fallback = fallback or t
    end
  end
  if fallback then fallback:activate() end
end)

local function cycle(direction)
  return wezterm.action_callback(function(window)
    local tabs = {}
    for _, t in ipairs(window:mux_window():tabs()) do
      if not is_agents(t) then table.insert(tabs, t) end
    end
    if #tabs == 0 then return end

    local active = window:active_tab()
    local active_id = active and active:tab_id() or nil
    local current
    for i, t in ipairs(tabs) do
      if t:tab_id() == active_id then
        current = i; break
      end
    end

    local target = current
        and ((current - 1 + direction) % #tabs) + 1
        or (direction > 0 and 1 or #tabs)
    tabs[target]:activate()
  end)
end

M.cycle_left = cycle(-1)
M.cycle_right = cycle(1)

local function move(direction)
  return wezterm.action_callback(function(window, pane)
    local active = window:active_tab()
    if not active or is_agents(active) then return end

    local tabs = window:mux_window():tabs()
    local idx
    for i, t in ipairs(tabs) do
      if t:tab_id() == active:tab_id() then
        idx = i - 1; break
      end
    end
    if not idx then return end

    local target = idx + direction
    if target < 0 or target >= #tabs then return end
    if is_agents(tabs[target + 1]) then target = target + direction end
    if target < 0 or target >= #tabs then return end

    window:perform_action(wezterm.action.MoveTab(target), pane)
  end)
end

M.move_left = move(-1)
M.move_right = move(1)

M.split_horizontal = wezterm.action({ SplitHorizontal = { domain = "CurrentPaneDomain" } })
M.split_vertical = wezterm.action({ SplitVertical = { domain = "CurrentPaneDomain" } })
M.split_right_35 = wezterm.action({ SplitPane = { direction = "Right", size = { Percent = 35 } } })

---Bold, coloured tab cell for the special tabs. Active tabs use the theme's
---active colours; inactive ones take <inactive_bg> so they stay visible.
---@param tab TabInformation
---@param config table
---@param glyph string
---@param title string
---@param inactive_bg string
local function highlighted_title(tab, config, glyph, title, inactive_bg)
  local colors = config.colors or {}
  local active = colors.tab_bar and colors.tab_bar.active_tab or {}
  return {
    { Background = { Color = tab.is_active and (active.bg_color or "#CBE3B3") or inactive_bg } },
    { Foreground = { Color = active.fg_color or "#171C1F" } },
    { Attribute = { Intensity = "Bold" } },
    { Text = " " .. glyph .. " " .. title .. " " },
  }
end

local function format_tab_title(tab, _, _, config)
  local title = tab.tab_title and tab.tab_title ~= "" and tab.tab_title
      or (tab.active_pane and tab.active_pane.title or "")

  if is_agents(tab) then
    return highlighted_title(tab, config, "✦", title ~= "" and title or M.TITLE, "#E69875")
  end

  if dev_tabs.is_dev(tab) then
    return highlighted_title(tab, config, "▸", title ~= "" and title or "dev", "#DBBC7F")
  end

  return " " .. title .. " "
end

M.register = function()
  wezterm.on("format-tab-title", format_tab_title)
end

return M
