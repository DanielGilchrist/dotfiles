local wezterm = require("wezterm")
local notify = require("utils.notify")

---@class AgentsTabModule
---@field TITLE string
---@field toggle Action
---@field cycle_left Action
---@field cycle_right Action
---@field move_left Action
---@field move_right Action
---@field close_pane Action
---@field close_tab Action
---@field split_horizontal Action
---@field split_vertical Action
---@field split_right_35 Action
---@field refuse fun(action: Action, message: string|nil): Action
---@field pin_to_zero Action
---@field register fun(): nil
local M = { TITLE = "agents" }

-- The agents tab is identified by its wezterm tab id, kept in wezterm.GLOBAL.
-- Survives config reloads; resets on wezterm restart (gui-startup repopulates).
-- Title is cosmetic — rename freely.
local function is_agents(tab)
  local id = wezterm.GLOBAL.agents_tab_id
  if type(id) ~= "number" or tab == nil then return false end
  -- MuxTab has :tab_id(); TabInformation has .tab_id (a number).
  local tid = type(tab.tab_id) == "function" and tab:tab_id() or tab.tab_id
  return tid == id
end

local function find_agents(window)
  for _, t in ipairs(window:mux_window():tabs()) do
    if is_agents(t) then return t end
  end
  wezterm.GLOBAL.agents_tab_id = nil -- stale; clear so callers can respawn
  return nil
end

-- macOS GUI apps don't inherit shell PATH; route through the user's login
-- shell so its profile (which sources Homebrew etc.) sets PATH up first.
local SHELL = os.getenv("SHELL") or "/bin/sh"

local function meta_session_alive()
  return wezterm.run_child_process({
    SHELL, "-l", "-c", "zellij list-sessions -s | grep -q '^agents$'"
  }) == true
end

-- MoveTab acts on the active tab, so we activate-move-restore.
local function pin_to_zero(window, tab)
  local prev = window:active_tab()
  tab:activate()
  window:perform_action(wezterm.action.MoveTab(0), window:active_pane())
  if prev and prev:tab_id() ~= tab:tab_id() then prev:activate() end
end

local function spawn_agents_tab(window)
  local tab = window:mux_window():spawn_tab({
    args = { SHELL, "-l", "-c", "exec zellij attach agents" },
  })
  if not tab then return end
  wezterm.GLOBAL.agents_tab_id = tab:tab_id()
  tab:set_title(M.TITLE)
  pin_to_zero(window, tab)
end

-- Find + pin, with one retry for the spawn → mux-registration race.
local function pin_with_retry(window)
  local function try()
    local agents = find_agents(window)
    if agents then pin_to_zero(window, agents); return true end
    return false
  end
  if not try() then wezterm.time.call_after(0.3, try) end
end

M.pin_to_zero = wezterm.action_callback(pin_with_retry)

M.toggle = wezterm.action_callback(function(window)
  local agents = find_agents(window)
  if not agents then
    if meta_session_alive() then
      spawn_agents_tab(window)
    else
      notify(window, "agent", "no agents — run `agent <name>` to spawn")
    end
    return
  end

  local active = window:active_tab()
  if not active then return end

  if active:tab_id() ~= agents:tab_id() then
    wezterm.GLOBAL.agent_prev_tab = active:tab_id()
    agents:activate()
    return
  end

  -- Returning from agents: prefer remembered tab, else first non-agents.
  local prev_id = wezterm.GLOBAL.agent_prev_tab
  local fallback
  for _, t in ipairs(window:mux_window():tabs()) do
    if not is_agents(t) then
      if t:tab_id() == prev_id then t:activate(); return end
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
      if t:tab_id() == active_id then current = i; break end
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
      if t:tab_id() == active:tab_id() then idx = i - 1; break end
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

local function refuse(action, message)
  local msg = message or "agents tab — operation refused"
  return wezterm.action_callback(function(window, pane)
    if is_agents(window:active_tab()) then
      notify(window, "agent", msg)
      return
    end
    window:perform_action(action, pane)
  end)
end

M.refuse = refuse

local CLOSE_MSG = "agents tab — use `agent-rm` to remove individual agents"
local SPLIT_MSG = "agents tab — pane splits disabled (single zellij viewport)"
M.close_pane = refuse(wezterm.action({ CloseCurrentPane = { confirm = false } }), CLOSE_MSG)
M.close_tab = refuse(wezterm.action({ CloseCurrentTab = { confirm = false } }), CLOSE_MSG)
M.split_horizontal = refuse(wezterm.action({ SplitHorizontal = { domain = "CurrentPaneDomain" } }), SPLIT_MSG)
M.split_vertical = refuse(wezterm.action({ SplitVertical = { domain = "CurrentPaneDomain" } }), SPLIT_MSG)
M.split_right_35 = refuse(wezterm.action({ SplitPane = { direction = "Right", size = { Percent = 35 } } }), SPLIT_MSG)

local function format_tab_title(tab, _, _, config)
  local title = tab.tab_title and tab.tab_title ~= "" and tab.tab_title
    or (tab.active_pane and tab.active_pane.title or "")
  if not is_agents(tab) then return " " .. title .. " " end

  local active = config.colors and config.colors.tab_bar and config.colors.tab_bar.active_tab or {}
  return {
    { Background = { Color = tab.is_active and (active.bg_color or "#CBE3B3") or "#E69875" } },
    { Foreground = { Color = active.fg_color or "#171C1F" } },
    { Attribute = { Intensity = "Bold" } },
    { Text = " ✦ " .. (title ~= "" and title or M.TITLE) .. " " },
  }
end

M.register = function()
  wezterm.on("format-tab-title", format_tab_title)
  -- Fish-side helpers emit `agents-tab-spawned=<pane_id>` from the *calling*
  -- shell pane after spawning the agents tab via `wezterm cli spawn`. We can't
  -- use `pane:tab()` from the event arg (that's the calling pane, not the new
  -- one), so resolve the agents tab via the new pane's id.
  wezterm.on("user-var-changed", function(window, _, name, value)
    if name ~= "agents-tab-spawned" then return end
    local target_pid = tonumber(value)
    if not target_pid then return end

    local function locate_and_pin()
      for _, t in ipairs(window:mux_window():tabs()) do
        for _, p in ipairs(t:panes()) do
          if p:pane_id() == target_pid then
            wezterm.GLOBAL.agents_tab_id = t:tab_id()
            pin_to_zero(window, t)
            return true
          end
        end
      end
      return false
    end
    if not locate_and_pin() then wezterm.time.call_after(0.3, locate_and_pin) end
  end)
  wezterm.on("gui-startup", function(cmd)
    -- gui-startup hands us mux objects, not a GUI window — so we can't reuse
    -- spawn_agents_tab here (it relies on perform_action). Instead, when the
    -- meta-session is alive, make the agents tab the window's *initial* tab
    -- (no MoveTab needed), then add the user's primary tab on top.
    if not meta_session_alive() then
      wezterm.mux.spawn_window(cmd or {})
      return
    end
    local agents_tab, _, mux_win = wezterm.mux.spawn_window({
      args = { SHELL, "-l", "-c", "exec zellij attach agents" },
    })
    if not agents_tab then
      wezterm.mux.spawn_window(cmd or {})
      return
    end
    wezterm.GLOBAL.agents_tab_id = agents_tab:tab_id()
    agents_tab:set_title(M.TITLE)
    local primary = mux_win:spawn_tab(cmd or {})
    if primary then primary:activate() end
  end)
end

return M
