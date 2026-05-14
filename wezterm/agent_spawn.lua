local wezterm = require("wezterm")

---@class AgentSpawnChoice
---@field label string
---@field id string

---@class AgentSpawnModule
---@field open fun(window: Window, pane: Pane): nil
---@field remove fun(window: Window, pane: Pane): nil
---@field edit_focused fun(window: Window, pane: Pane): nil
local M = {}

local REPOS_DIR = wezterm.home_dir .. "/Documents/repos"

-- wezterm's `SpawnCommandInNewTab` spawns with a minimal PATH (no
-- /opt/homebrew/bin), so plain "fish" doesn't resolve. Resolve on first use —
-- `run_child_process` can't be called at module-load time (no coroutine ctx).
local fish_path
local function resolve_fish()
  if fish_path then return fish_path end
  local _, out = wezterm.run_child_process({ "/bin/sh", "-lc", "command -v fish" })
  if out then
    local path = out:gsub("%s+$", "")
    if path ~= "" then
      fish_path = path
      return fish_path
    end
  end
  fish_path = "/opt/homebrew/bin/fish"
  return fish_path
end

---@return AgentSpawnChoice[]
local function list_repos()
  ---@type AgentSpawnChoice[]
  local choices = {}
  local _, out = wezterm.run_child_process({
    "find", REPOS_DIR,
    "-mindepth", "2", "-maxdepth", "2", "-type", "d",
  })
  if not out then return choices end

  for path in out:gmatch("[^\n]+") do
    local has_git = wezterm.run_child_process({ "test", "-e", path .. "/.git" })
    if has_git then
      local org, repo = path:match("/repos/([^/]+)/([^/]+)$")
      if org and repo then
        table.insert(choices, { label = org .. "/" .. repo, id = path })
      end
    end
  end
  return choices
end

---Single-quote a string for safe inclusion in a fish-shell command.
---@param s string
---@return string
local function fish_quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

---@param repo_path string
---@param name string
---@return string
local function build_spawn_cmd(repo_path, name)
  -- agent.fish opens nvim for the prompt when no -e/--seed is given and the
  -- worktree doesn't yet exist. When :wq closes nvim, agent.fish finishes
  -- spawning into the meta-session and exits, taking this temporary tab with
  -- it. cd inside fish rather than trusting wezterm's --cwd (canonicalisation
  -- bug, wez/wezterm#4618).
  return "cd " .. fish_quote(repo_path) .. "; and agent " .. fish_quote(name)
end

M.open = function(window, pane)
  window:perform_action(wezterm.action.InputSelector({
    title = "Pick a repo for the new agent",
    choices = list_repos(),
    fuzzy = true,
    action = wezterm.action_callback(function(repo_window, repo_pane, repo_path)
      if not repo_path then return end

      repo_window:perform_action(wezterm.action.PromptInputLine({
        description = "agent name (kebab-case, ≤25 chars):",
        action = wezterm.action_callback(function(name_window, name_pane, line)
          if not line then return end
          local name = line:match("^%s*(.-)%s*$") or ""
          if name == "" then return end

          name_window:perform_action(wezterm.action.SpawnCommandInNewTab({
            -- `-i` makes `status --is-interactive` true inside fish, which is
            -- what agent.fish checks before opening nvim for the seed prompt.
            args = { resolve_fish(), "-i", "-c", build_spawn_cmd(repo_path, name) },
          }), name_pane)
        end),
      }), repo_pane)
    end),
  }), pane)
end

---@return AgentSpawnChoice[]
local function list_agents()
  ---@type AgentSpawnChoice[]
  local choices = {}
  -- Match worktrees on disk rather than zellij sessions: the worktree is the
  -- source of truth (sessions can die independently), and we want the picker
  -- to surface "agents I can remove" even if zellij forgot one.
  local _, out = wezterm.run_child_process({
    "find", wezterm.home_dir .. "/worktrees",
    "-mindepth", "2", "-maxdepth", "2", "-type", "d",
  })
  if not out then return choices end

  for path in out:gmatch("[^\n]+") do
    local repo, branch = path:match("/worktrees/([^/]+)/([^/]+)$")
    if repo and branch then
      table.insert(choices, { label = repo .. "/" .. branch, id = branch })
    end
  end
  return choices
end

M.remove = function(window, pane)
  window:perform_action(wezterm.action.InputSelector({
    title = "Pick an agent to remove (--force)",
    choices = list_agents(),
    fuzzy = true,
    action = wezterm.action_callback(function(inner_window, inner_pane, branch)
      if not branch then return end
      inner_window:perform_action(wezterm.action.SpawnCommandInNewTab({
        args = { resolve_fish(), "-c", "agent-rm " .. fish_quote(branch) .. " --force" },
      }), inner_pane)
    end),
  }), pane)
end

M.edit_focused = function(window, pane)
  -- Ask zellij directly which agent pane is currently focused. The helper
  -- cross-references `current-tab-info` (focused tab) with `list-panes`
  -- (is_focused is per-tab in zellij 0.44, so we constrain by tab_id).
  local _, out = wezterm.run_child_process({
    resolve_fish(), "-c", "_agent_focused_worktree",
  })
  local cwd = out and out:gsub("%s+$", "") or ""
  if cwd == "" then
    window:toast_notification("agent", "no focused agent", nil, 2000)
    return
  end

  -- Spawn the editor directly with wezterm's cwd option — no shell wrapper
  -- needed. wezterm records the spawn cwd on the pane, which the default tab
  -- title format renders as "<editor> <cwd>".
  -- Route through fish so the editor inherits the user's full env (PATH,
  -- shell helpers, etc.) — wezterm's spawn env is too minimal for nvim
  -- plugins that shell out to rg/fd/etc. `exec` replaces fish with the
  -- editor so the pane process is the editor, not fish. `label` sets the
  -- tab title up front so we don't see "fish" flash before exec.
  local short_cwd = cwd:gsub("^" .. wezterm.home_dir, "~")
  window:perform_action(wezterm.action.SpawnCommandInNewTab({
    label = "nvim " .. short_cwd,
    args = {
      resolve_fish(), "-i", "-c",
      "set -q EDITOR; or set EDITOR nvim; cd " .. fish_quote(cwd) .. "; and exec $EDITOR",
    },
  }), pane)
end

return M
