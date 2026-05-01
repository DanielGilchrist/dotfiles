local wezterm = require("wezterm")

---@class AgentsTabModule
---@field TITLE string
---@field toggle Action
---@field cycle_left Action
---@field cycle_right Action
---@field move_left Action
---@field move_right Action
---@field close_pane Action
---@field close_tab Action
---@field pin_to_zero Action
---@field register fun(): nil
local M = {}

M.TITLE = "agents"
M.MARKER_FILE = "/tmp/wezterm-agents-tab"

---Read the persisted agents tab id from disk. Cold-start fallback only.
---@return integer|nil
local function read_marker_file()
  local f = io.open(M.MARKER_FILE, "r")
  if not f then return nil end
  local raw = f:read("*l")
  f:close()
  if not raw then return nil end
  return tonumber(raw)
end

---Cached lookup. `wezterm.GLOBAL.agents_tab_id` is populated by the
---`agents-tab-id` user-var event (emitted from `agent.fish`). Falls back
---to a one-time disk read so we still recover the id after a wezterm
---restart (the file persists in /tmp until reboot or first new spawn).
---@return integer|nil
local function get_marker()
  local id = wezterm.GLOBAL.agents_tab_id
  if type(id) == "number" then return id end
  id = read_marker_file()
  if id then wezterm.GLOBAL.agents_tab_id = id end
  return id
end

---@param tab_id integer|nil
---@return boolean
local function is_agents_tab_id(tab_id)
  if not tab_id then return false end
  local marker = get_marker()
  return marker ~= nil and marker == tab_id
end

---@param tabs MuxTab[]
---@param tab_id integer
---@return integer|nil zero-based index, or nil if not found
local function index_of(tabs, tab_id)
  for i, t in ipairs(tabs) do
    if t:tab_id() == tab_id then return i - 1 end
  end
  return nil
end

---@param tabs MuxTab[]
---@return MuxTab|nil mux_tab
---@return integer|nil zero-based index
local function find_agents_tab(tabs)
  local marker = get_marker()
  if not marker then return nil, nil end
  for i, t in ipairs(tabs) do
    if t:tab_id() == marker then return t, i - 1 end
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
    if type(prev_id) == "number" then
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
---@return Action
local function cycle(direction)
  return wezterm.action_callback(function(window, pane)
    local tabs = window:mux_window():tabs()
    local agents = find_agents_tab(tabs)
    local agents_id = agents and agents:tab_id() or nil

    ---@type {tab: MuxTab, idx: integer}[]
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
---@return Action
local function move(direction)
  return wezterm.action_callback(function(window, pane)
    local active = window:active_tab()
    if not active or is_agents_tab_id(active:tab_id()) then return end

    local tabs = window:mux_window():tabs()
    local active_idx = index_of(tabs, active:tab_id())
    if not active_idx then return end
    local target_idx = active_idx + direction
    if target_idx < 0 or target_idx >= #tabs then return end

    if is_agents_tab_id(tabs[target_idx + 1]:tab_id()) then
      target_idx = target_idx + direction
    end
    if target_idx < 0 or target_idx >= #tabs then return end

    window:perform_action(wezterm.action.MoveTab(target_idx), pane)
  end)
end

M.move_left = move(-1)
M.move_right = move(1)

---Block a close action when the active tab is the agents tab. Otherwise
---perform the action.
---@param close_action Action
---@return Action
local function refuse_on_agents_tab(close_action)
  return wezterm.action_callback(function(window, pane)
    local tab = window:active_tab()
    if tab and is_agents_tab_id(tab:tab_id()) then
      window:toast_notification("agent", "agents tab — use `agent-rm` to remove", nil, 2000)
      return
    end
    window:perform_action(close_action, pane)
  end)
end

M.close_pane = refuse_on_agents_tab(wezterm.action({ CloseCurrentPane = { confirm = false } }))
M.close_tab = refuse_on_agents_tab(wezterm.action({ CloseCurrentTab = { confirm = false } }))

---@param window Window
---@param pane Pane
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

---@param tab TabInformation
---@param config Config
---@return string|table
local function format_tab_title(tab, _, _, config)
  local title = tab.tab_title and tab.tab_title ~= "" and tab.tab_title or (tab.active_pane and tab.active_pane.title or "")
  if is_agents_tab_id(tab.tab_id) then
    -- Active: same as a regular active tab so the agents tab blends in when
    -- focused. Inactive: orange accent so it's distinguishable from normal
    -- tabs at a glance. Bold either way to make it pop.
    local active_tab = config.colors and config.colors.tab_bar and config.colors.tab_bar.active_tab or {}
    local active_bg = active_tab.bg_color or "#CBE3B3"
    local fg = active_tab.fg_color or "#171C1F"
    local bg = tab.is_active and active_bg or "#E69875"
    return {
      { Background = { Color = bg } },
      { Foreground = { Color = fg } },
      { Attribute = { Intensity = "Bold" } },
      { Text = " ✦ " .. M.TITLE .. " " },
    }
  end
  return " " .. title .. " "
end

---@param window Window
---@param session_name string
---@return MuxTab|nil tab
---@return Pane|nil pane
local function find_agent_pane(window, session_name)
  -- Match by pane cwd (~/worktrees/<repo>/<session>/) so this works even
  -- when the program in the pane (claude) has overridden the pane title.
  local cwd_suffix = "/" .. session_name .. "/"
  local cwd_suffix_no_slash = "/" .. session_name
  for _, t in ipairs(window:mux_window():tabs()) do
    if is_agents_tab_id(t:tab_id()) then
      for _, p in ipairs(t:panes()) do
        local cwd = p:get_current_working_dir()
        local path = ""
        if cwd then
          path = type(cwd) == "string" and cwd or (cwd.file_path or "")
        end
        if path ~= "" then
          if path:sub(-#cwd_suffix) == cwd_suffix or path:sub(-#cwd_suffix_no_slash) == cwd_suffix_no_slash then
            return t, p
          end
        end
        -- Fall back to title match in case cwd isn't populated yet.
        local title = p:get_title() or ""
        if title:find(session_name, 1, true) then
          return t, p
        end
      end
    end
  end
  return nil, nil
end

---@param window Window
---@param session_name string
---@param zoomed boolean
local function set_agent_pane_zoom(window, session_name, zoomed)
  local tab, target = find_agent_pane(window, session_name)
  if not tab or not target then return end

  -- pane:activate() forces the tab to foreground, which would yank the
  -- user away from whatever tab they were on. Snapshot the active tab
  -- first and restore it after zooming.
  local prev_tab = window:active_tab()
  target:activate()
  tab:set_zoomed(zoomed)
  if prev_tab and prev_tab:tab_id() ~= tab:tab_id() then
    local idx = index_of(window:mux_window():tabs(), prev_tab:tab_id())
    if idx then
      window:perform_action(wezterm.action.ActivateTab(idx), window:active_pane())
    end
  end
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
    elseif name == "agents-tab-id" then
      local id = tonumber(value)
      if id then wezterm.GLOBAL.agents_tab_id = id end
    end
  end)
end

return M
