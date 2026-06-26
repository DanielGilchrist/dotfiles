local wezterm = require("wezterm")
local notify = require("utils.notify")
local agent_spawn = require("agent_spawn")

local M = {}

---@class (exact) PaneDirection
---@field Right string
---@field Bottom string

---@type PaneDirection
local pane_direction = {
  Right = "Right",
  Bottom = "Bottom",
}

---@class (exact) Commands
---@field CDT string
---@field Console string
---@field Server string
---@field Start string
---@field Tunnel string
---@field Worker string
---@field Webpack string

---@type Commands
local commands = {
  CDT = "cdt",
  Console = "bin/dev console",
  Server = "bin/dev server",
  Start = "bin/dev start",
  Tunnel = "bin/tunnel",
  Worker = "bin/dev worker",
  Webpack = "bin/dev webpack",
}

---@class (exact) Region
---@field APAC string
---@field EU string

---@type Region
local regions = {
  APAC = "apac",
  EU = "eu",
  US = "us"
}

local function split_pane_with(func)
  return function(pane, direction, size)
    size = size == nil and 0.5 or size
    local new_pane = pane:split({ direction = direction, size = size })
    func(new_pane)
    return new_pane
  end
end

local function has_text(pane, ...)
  local text = pane:get_logical_lines_as_text(pane:get_dimensions().scrollback_rows)

  for i = 1, select("#", ...) do
    local pattern = select(i, ...)

    if string.find(text, pattern) then
      return true
    end
  end

  return false
end

local function run_command(pane, command)
  pane:send_text(command .. "\n")
end

local function run_commands(pane, ...)
  local commands_table = { ... }
  local joined_command = table.concat(commands_table, " && ")

  run_command(pane, joined_command)
end

local function wait_for_text_for(pane, ...)
  while true do
    if has_text(pane, ...) then
      break
    else
      wezterm.sleep_ms(1000)
    end
  end
end

local function clear(pane)
  pane:send_text("\x0c")
end

local function has_interrupted_system_call(pane)
  return has_text(pane, "cd: Interrupted system call")
end

local function buffered_cdt(pane)
  pane:send_text(commands.CDT)
  wezterm.sleep_ms(100)
  pane:send_text("\n")
end

local function wait_for_text()
  wezterm.sleep_ms(100)
end

local function handle_potential_interrupted_system_call_from_cdt(pane, already_tried)
  if has_interrupted_system_call(pane) then
    if already_tried then
      notify(pane.window, "OpenWorkTabs", "Error! Can't continue due to issue with executing `cdt`")
      return true
    else
      clear(pane)
      buffered_cdt(pane)
      wait_for_text()
      return handle_potential_interrupted_system_call_from_cdt(pane, true)
    end
  else
    return
  end
end

local function handle_missing_cdt_command(pane)
  if has_text(pane, "Unknown command") then
    notify(pane.window, "OpenWorkTabs", "Error! Please set a `cdt` alias")
    return true
  end
end

---@param region string
---@param cd_command string
---@return string
local function dev_tab_title(region, cd_command)
  -- Always prefix with region so two dev tabs in different regions are
  -- visually distinct. For `cd <path>` (worktree variant), append the
  -- directory name so the title makes it obvious which worktree the stack
  -- is rooted in.
  local path = cd_command:match("^cd%s+(.+)$")
  if path then
    path = path:gsub("^['\"]", ""):gsub("['\"]$", "")
    local basename = path:match("([^/]+)/?$")
    if basename and basename ~= "" then return "dev:" .. region .. ":" .. basename end
  end
  return "dev:" .. region
end

---Close the previously-spawned dev tab for <region> if it's still alive.
---Tracked per-region via wezterm.GLOBAL.dev_tab_id_by_region so each region
---gets its own dev tab — spawning a US tab won't tear down an existing EU
---tab, and vice versa.
---@param window any wezterm Window
---@param region string
local function close_existing_dev_tab(window, region)
  local by_region = wezterm.GLOBAL.dev_tab_id_by_region or {}
  local tab_id = by_region[region]
  if not tab_id then return end
  for _, tab in ipairs(window:mux_window():tabs()) do
    if tab:tab_id() == tab_id then
      for _, pane in ipairs(tab:panes()) do
        wezterm.run_child_process({ "/opt/homebrew/bin/wezterm", "cli", "kill-pane", "--pane-id", tostring(pane:pane_id()) })
      end
      break
    end
  end
  by_region[region] = nil
  wezterm.GLOBAL.dev_tab_id_by_region = by_region
end

---@param original_window any
---@param region string
---@param cd_command string
local function spawn_dev_tab(original_window, region, cd_command)
    local use_cdt = cd_command == commands.CDT

    local function setup_pane(pane)
      run_commands(pane, "export REGION=" .. region, cd_command)
    end

    local split_pane_with_setup = split_pane_with(setup_pane)

    local new_tab, server_pane, window = original_window:mux_window():spawn_tab({})
    new_tab:set_title(dev_tab_title(region, cd_command))
    local by_region = wezterm.GLOBAL.dev_tab_id_by_region or {}
    by_region[region] = new_tab:tab_id()
    wezterm.GLOBAL.dev_tab_id_by_region = by_region

    local gui_window = window:gui_window()
    require("utils.tab").move_to_first(gui_window, server_pane)

    setup_pane(server_pane)
    wait_for_text()

    if use_cdt then
      if handle_missing_cdt_command(server_pane) then
        return
      end

      if handle_potential_interrupted_system_call_from_cdt(server_pane) then
        return
      end
    end

    local console_pane = split_pane_with_setup(server_pane, pane_direction.Right)
    local webpack_pane = split_pane_with_setup(console_pane, pane_direction.Bottom, 0.1)
    local tunnel_pane = split_pane_with_setup(server_pane, pane_direction.Bottom, 0.1)
    local worker_pane = split_pane_with_setup(server_pane, pane_direction.Bottom, 0.4)

    wait_for_text_for(tunnel_pane, "Welcome to fish")

    if region == regions.APAC or region == regions.EU then
      run_command(tunnel_pane, commands.Start)
      wait_for_text_for(tunnel_pane, "Your dev box", "is ready to be used")
    end

    run_command(tunnel_pane, commands.Tunnel)
    wait_for_text_for(tunnel_pane, "Enter your developer name:", "INFO: Ready!")

    if has_text(tunnel_pane, "Enter your developer name:") then
      run_command(tunnel_pane, "dangilchrist")
      wait_for_text_for(tunnel_pane, "INFO: Ready!")
    end

    run_command(server_pane, commands.Server)
    wait_for_text_for(server_pane, "Done in", "Running server at")

    run_command(webpack_pane, commands.Webpack)
    run_command(console_pane, commands.Console)
    run_command(worker_pane, commands.Worker)
end

---Re-entrant wrapper around spawn_dev_tab. Refuses while a spawn is in
---flight for the same region (open_work_environment yields at every
---wait_for_text, so two same-region spawns would race and clobber each
---other's panes). Different regions can spawn in parallel.
local function open_work_environment(region, cd_command)
  return function(original_window, _original_pane, _line)
    local in_progress = wezterm.GLOBAL.dev_spawn_in_progress_by_region or {}
    if in_progress[region] then
      original_window:toast_notification("dev", "dev tab for " .. region .. " is already spawning — wait for it to finish", nil, 2500)
      return
    end

    close_existing_dev_tab(original_window, region)
    in_progress[region] = true
    wezterm.GLOBAL.dev_spawn_in_progress_by_region = in_progress
    local ok, err = pcall(spawn_dev_tab, original_window, region, cd_command)
    in_progress = wezterm.GLOBAL.dev_spawn_in_progress_by_region or {}
    in_progress[region] = nil
    wezterm.GLOBAL.dev_spawn_in_progress_by_region = in_progress
    if not ok then error(err) end
  end
end

local function worktree_choices()
  local worktrees_dir = wezterm.home_dir .. "/worktrees"
  -- New layout: ~/worktrees/<repo>/<branch>. Find one level deep so we get
  -- every branch across every repo.
  local success, stdout, _stderr = wezterm.run_child_process({
    "find", worktrees_dir, "-mindepth", "2", "-maxdepth", "2", "-type", "d",
  })

  if not success or stdout == "" then
    return {}
  end

  local choices = {}
  for path in stdout:gmatch("[^\n]+") do
    local repo, branch = path:match("/worktrees/([^/]+)/([^/]+)$")
    if repo and branch then
      table.insert(choices, { label = repo .. "/" .. branch, id = repo .. "/" .. branch })
    end
  end

  return choices
end

local function open_worktree_selector()
  return function(window, pane)
    window:perform_action(wezterm.action.InputSelector({
      title = "Select a worktree",
      choices = worktree_choices(),
      action = wezterm.action_callback(function(inner_window, inner_pane, id, _label)
        if not id then
          return
        end

        local cd_command = "cd ~/worktrees/" .. id
        open_work_environment(regions.US, cd_command)(inner_window, inner_pane)
      end),
    }), pane)
  end
end

---Single-quote a string for safe inclusion in a fish-shell command.
---@param s string
---@return string
local function fish_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

---@param window any wezterm Window
---@return string|nil
local function focused_agent_worktree(window)
  local fish = "/opt/homebrew/bin/fish"
  local _, out = wezterm.run_child_process({ fish, "-c", "_agent_focused_worktree" })
  if not out then return nil end
  local cwd = out:gsub("%s+$", "")
  if cwd == "" then
    window:toast_notification("dev", "no focused agent", nil, 2000)
    return nil
  end
  return cwd
end

---Pop the region picker, then run open_work_environment with cd_command.
---@param window any
---@param pane any
---@param cd_command string
local function pick_region_and_spawn(window, pane, cd_command)
  window:perform_action(wezterm.action.InputSelector({
    title = "Region for dev tabs",
    choices = {
      { label = "[US] (default)", id = regions.US },
      { label = "[EU]",           id = regions.EU },
      { label = "[APAC]",         id = regions.APAC },
    },
    fuzzy = true,
    action = wezterm.action_callback(function(inner_window, _, region)
      if not region then return end
      local target = inner_window:active_pane()
      if not target then return end
      open_work_environment(region, cd_command)(inner_window, target)
    end),
  }), pane)
end

M.open_work_in_focused_agent = function(window, pane)
  local cwd = focused_agent_worktree(window)
  if not cwd then return end

  -- Dev-server commands (bin/dev, bin/tunnel, cdt) are payaus-only. Refuse
  -- for worktrees under any other repo.
  local repo = cwd:match("/worktrees/([^/]+)/[^/]+/?$")
  if repo ~= "payaus" then
    window:toast_notification("dev", "dev server is payaus-only (focused: " .. (repo or "?") .. ")", nil, 3000)
    return
  end

  pick_region_and_spawn(window, pane, "cd " .. fish_quote(cwd))
end

-- Listen for `agent-spawn-dev=<region>|<cwd>` (emitted by nvim with the
-- region already chosen via vim.ui.select). If <cwd> is under
-- ~/worktrees/<payaus>/* we cd into the worktree; otherwise we fall back
-- to `cdt`. Other repos are refused (dev server is payaus-only).
wezterm.on("user-var-changed", function(window, pane, name, value)
  if name ~= "agent-spawn-dev" then return end
  local region, cwd = (value or ""):match("^([^|]+)|(.*)$")
  if not region or not cwd then
    window:toast_notification("dev", "agent-spawn-dev: malformed payload", nil, 3000)
    return
  end
  window:toast_notification("dev", ("recv region=%s cwd=%s"):format(region, cwd), nil, 3000)

  local worktree_root = wezterm.home_dir .. "/worktrees/"
  local cd_command
  if cwd:sub(1, #worktree_root) == worktree_root then
    local repo = cwd:match("/worktrees/([^/]+)/[^/]+/?$")
    if repo ~= "payaus" then
      window:toast_notification("dev", "dev server is payaus-only (got: " .. (repo or "?") .. ")", nil, 3000)
      return
    end
    cd_command = "cd " .. fish_quote(cwd)
  else
    cd_command = commands.CDT
  end

  open_work_environment(region, cd_command)(window, pane)
end)

M.register_commands = function()
  wezterm.on("augment-command-palette", function(_window, _pane)
    return {
      {
        brief = "[APAC] Open work tabs",
        action = wezterm.action_callback(open_work_environment(regions.APAC, commands.CDT)),
      },
      {
        brief = "[EU] Open work tabs",
        action = wezterm.action_callback(open_work_environment(regions.EU, commands.CDT)),
      },
      {
        brief = "[US] Open work tabs",
        action = wezterm.action_callback(open_work_environment(regions.US, commands.CDT)),
      },
      {
        brief = "[US] Open worktree tabs",
        action = wezterm.action_callback(open_worktree_selector()),
      },
      {
        brief = "Spawn agent in repo",
        action = wezterm.action_callback(agent_spawn.open),
      },
      {
        brief = "Remove agent",
        action = wezterm.action_callback(agent_spawn.remove),
      },
    }
  end)
end

return M
