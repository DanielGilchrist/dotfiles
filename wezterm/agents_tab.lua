local wezterm = require("wezterm")

---@class AgentsTabModule
---@field TITLE string
---@field toggle any
---@field cycle_left any
---@field cycle_right any
---@field move_left any
---@field move_right any
---@field pin_to_zero any
---@field register fun(): nil
local M = {}

M.TITLE = "agents"

---@param tabs table[]
---@param tab_id integer
---@return integer|nil zero-based index, or nil if not found
local function index_of(tabs, tab_id)
  for i, t in ipairs(tabs) do
    if t:tab_id() == tab_id then return i - 1 end
  end
  return nil
end

---@param tabs table[]
---@return any|nil mux_tab
---@return integer|nil zero-based index
local function find_agents_tab(tabs)
  for i, t in ipairs(tabs) do
    if t:get_title() == M.TITLE then return t, i - 1 end
  end
  return nil, nil
end

M.toggle = wezterm.action_callback(function(window, pane)
  local tabs = window:mux_window():tabs()
  local agents, agents_idx = find_agents_tab(tabs)
  if not agents then
    window:toast_notification("agent", "no agents tab — run `agent <name>`", nil, 2000)
    return
  end

  local active = window:active_tab()
  local active_id = active and active:tab_id() or nil

  if active_id == agents:tab_id() then
    local prev_id = wezterm.GLOBAL.agent_prev_tab
    if prev_id then
      local prev_idx = index_of(tabs, prev_id)
      if prev_idx then
        window:perform_action(wezterm.action.ActivateTab(prev_idx), pane)
        return
      end
    end
    for i, t in ipairs(tabs) do
      if t:tab_id() ~= agents:tab_id() then
        window:perform_action(wezterm.action.ActivateTab(i - 1), pane)
        return
      end
    end
    return
  end

  if active_id then wezterm.GLOBAL.agent_prev_tab = active_id end
  window:perform_action(wezterm.action.ActivateTab(agents_idx), pane)
end)

---@param direction integer
---@return any
local function cycle(direction)
  return wezterm.action_callback(function(window, pane)
    local tabs = window:mux_window():tabs()
    local agents = find_agents_tab(tabs)
    local agents_id = agents and agents:tab_id() or nil

    ---@type {tab: any, idx: integer}[]
    local cyclable = {}
    for i, t in ipairs(tabs) do
      if t:tab_id() ~= agents_id then
        table.insert(cyclable, { tab = t, idx = i - 1 })
      end
    end
    if #cyclable == 0 then return end

    local active = window:active_tab()
    local active_id = active and active:tab_id() or nil

    local current = nil
    for i, c in ipairs(cyclable) do
      if c.tab:tab_id() == active_id then current = i break end
    end

    local target
    if not current then
      target = direction > 0 and cyclable[1] or cyclable[#cyclable]
    else
      local next_idx = ((current - 1 + direction) % #cyclable) + 1
      target = cyclable[next_idx]
    end

    window:perform_action(wezterm.action.ActivateTab(target.idx), pane)
  end)
end

M.cycle_left = cycle(-1)
M.cycle_right = cycle(1)

---@param direction integer
---@return any
local function move(direction)
  return wezterm.action_callback(function(window, pane)
    local active = window:active_tab()
    if not active or active:get_title() == M.TITLE then return end

    local tabs = window:mux_window():tabs()
    local active_idx = index_of(tabs, active:tab_id())
    if not active_idx then return end
    local target_idx = active_idx + direction
    if target_idx < 0 or target_idx >= #tabs then return end

    if tabs[target_idx + 1]:get_title() == M.TITLE then
      target_idx = target_idx + direction
    end
    if target_idx < 0 or target_idx >= #tabs then return end

    window:perform_action(wezterm.action.MoveTab(target_idx), pane)
  end)
end

M.move_left = move(-1)
M.move_right = move(1)

---@param window any
---@param pane any
local function do_pin_to_zero(window, pane)
  local function attempt()
    local tabs = window:mux_window():tabs()
    local agents, agents_idx = find_agents_tab(tabs)
    if not agents then return false end
    local current = window:active_tab()
    if not current then return true end
    local prev_id = current:tab_id() ~= agents:tab_id() and current:tab_id() or nil

    if current:tab_id() ~= agents:tab_id() then
      window:perform_action(wezterm.action.ActivateTab(agents_idx), pane)
    end
    window:perform_action(wezterm.action.MoveTab(0), pane)

    if prev_id then
      local prev_idx = index_of(window:mux_window():tabs(), prev_id)
      if prev_idx then
        window:perform_action(wezterm.action.ActivateTab(prev_idx), pane)
      end
    end
    return true
  end

  -- Retry to dodge the race where the agents tab isn't yet registered with
  -- the mux at the moment the user-var event fires.
  if attempt() then return end
  for _, delay in ipairs({ 0.1, 0.3, 0.6, 1.0 }) do
    wezterm.time.call_after(delay, function()
      if attempt() then return end
    end)
  end
end

M.pin_to_zero = wezterm.action_callback(do_pin_to_zero)

---@param tab any
---@return string|table
local function format_tab_title(tab)
  local title = tab.tab_title and tab.tab_title ~= "" and tab.tab_title or (tab.active_pane and tab.active_pane.title or "")
  if title == M.TITLE then
    return {
      { Background = { Color = "#E69875" } },
      { Foreground = { Color = "#171C1F" } },
      { Attribute = { Intensity = "Bold" } },
      { Text = " ✦ " .. M.TITLE .. " " },
    }
  end
  return " " .. title .. " "
end

---@param window any
---@param session_name string
---@return any|nil tab
---@return any|nil pane
local function find_agent_pane(window, session_name)
  for _, t in ipairs(window:mux_window():tabs()) do
    if t:get_title() == M.TITLE then
      for _, p in ipairs(t:panes()) do
        local title = p:get_title() or ""
        if title:find(session_name, 1, true) then
          return t, p
        end
      end
    end
  end
  return nil, nil
end

---@param window any
---@param session_name string
---@param zoomed boolean
local function set_agent_pane_zoom(window, session_name, zoomed)
  local tab, target = find_agent_pane(window, session_name)
  if not tab or not target then return end
  target:activate()
  tab:set_zoomed(zoomed)
end

M.register = function()
  wezterm.on("format-tab-title", format_tab_title)
  wezterm.on("user-var-changed", function(window, pane, name, value)
    if name == "agent-action" and value == "pin-agents-tab" then
      do_pin_to_zero(window, pane)
    elseif name == "agent-zoom" then
      set_agent_pane_zoom(window, value, true)
    elseif name == "agent-unzoom" then
      set_agent_pane_zoom(window, value, false)
    end
  end)
end

return M
