local wezterm = require("wezterm")
local notify = require("utils.notify")
local path_utils = require("utils.path")
local shell = require("utils.shell")
local worktree = require("utils.worktree")

---@class AgentSpawnChoice
---@field label string
---@field id string

---@class AgentSpawnModule
---@field open fun(window: Window, pane: Pane): nil
---@field remove fun(window: Window, pane: Pane): nil
---@field focused_worktree fun(window: Window): string|nil
---@field edit_focused fun(window: Window, pane: Pane): nil
---@field remove_focused fun(window: Window, pane: Pane): nil
---@field minimise_focused fun(window: Window, pane: Pane): nil
---@field attach_picker fun(window: Window, pane: Pane): nil
local M = {}

---Spawn a tab running <fish_cmd> through the user's fish so it inherits the
---full env (PATH, shell helpers). `-i` makes `status --is-interactive` true,
---which agent.fish checks before opening nvim for a seed prompt.
---@param window Window
---@param pane Pane
---@param fish_cmd string
---@param interactive? boolean
local function spawn_fish_tab(window, pane, fish_cmd, interactive)
  local args = { shell.fish() }
  if interactive then table.insert(args, "-i") end
  table.insert(args, "-c")
  table.insert(args, fish_cmd)

  window:perform_action(wezterm.action.SpawnCommandInNewTab({ args = args }), pane)
end

---@return AgentSpawnChoice[]
local function list_repos()
  ---@type AgentSpawnChoice[]
  local choices = {}
  for _, dir in ipairs(worktree.git_dirs(worktree.REPOS_DIR)) do
    local org, repo = dir:match("/repos/([^/]+)/([^/]+)$")
    if org and repo then
      table.insert(choices, { label = org .. "/" .. repo, id = dir })
    end
  end
  return choices
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
  return "cd " .. shell.quote(repo_path) .. "; and agent attach " .. shell.quote(name)
end

M.open = function(window, pane)
  window:perform_action(wezterm.action.InputSelector({
    title = "Pick a repo for the new agent",
    choices = list_repos(),
    fuzzy = true,
    action = wezterm.action_callback(function(repo_window, _, repo_path)
      if not repo_path then return end

      repo_window:perform_action(wezterm.action.PromptInputLine({
        description = "agent name (kebab-case, ≤20 chars):",
        action = wezterm.action_callback(function(name_window, _, line)
          if not line then return end
          local name = shell.trim(line):match("^%s*(.-)$")
          if name == "" then return end

          -- Resolve a fresh active pane: the pane that started this flow may
          -- have closed in between (PromptInputLine closes its own overlay
          -- pane on submit, and the captured pane is often stale by here —
          -- "pane id N is not valid" otherwise).
          local target = name_window:active_pane()
          if not target then return end

          spawn_fish_tab(name_window, target, build_spawn_cmd(repo_path, name), true)
        end),
      }), repo_window:active_pane())
    end),
  }), pane)
end

---@return AgentSpawnChoice[]
local function list_agents()
  ---@type AgentSpawnChoice[]
  local choices = {}
  for _, entry in ipairs(worktree.list()) do
    table.insert(choices, { label = entry.repo .. "/" .. entry.branch, id = entry.branch })
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
      spawn_fish_tab(inner_window, inner_pane, "agent rm --force " .. shell.quote(branch))
    end),
  }), pane)
end

---@param window any
---@return string|nil worktree dir of the focused agent pane, or nil with a toast if none
M.focused_worktree = function(window)
  -- Ask zellij which agent pane is focused (via `list-clients`), then
  -- resolve its worktree from the pane title → ~/worktrees/<repo>/<name>.
  -- The helper only returns directories that exist.
  local ok, cwd = shell.run({ shell.fish(), "-c", "_agent_focused_worktree" })
  if not ok or cwd == "" then
    notify(window, "agent", "no focused agent (or its worktree is gone)")
    return nil
  end
  return cwd
end

---Fish snippet that `cd`s into <cwd> and falls back to an interactive shell
---with the error visible when it can't, rather than letting the tab close
---before anything is readable.
---@param cwd string
---@return string
local function cd_or_shell(cwd)
  local quoted = shell.quote(cwd)
  return "cd " .. quoted .. "; or begin; echo 'agent: cannot cd into '" .. quoted .. "; exec fish -i; end"
end

M.edit_focused = function(window, pane)
  local cwd = M.focused_worktree(window)
  if not cwd then return end

  -- `exec` replaces fish with the editor so the pane process is the editor,
  -- not fish. The tab title comes from nvim itself (`title`/`titlestring` in
  -- config/tabs.lua) so it tracks the active tab page — an explicit wezterm
  -- tab_title would stick.
  spawn_fish_tab(window, pane, "set -q EDITOR; or set EDITOR nvim; " .. cd_or_shell(cwd) .. "; exec $EDITOR", true)
end

M.remove_focused = function(window, pane)
  local cwd = M.focused_worktree(window)
  if not cwd then return end
  local branch = path_utils.basename(cwd)
  if branch == "" then return end

  -- agent rm without --force refuses on dirty branches; user can rerun with
  -- --force from a regular pane if needed. Tab closes when the command exits.
  spawn_fish_tab(window, pane, "agent rm " .. shell.quote(branch), true)
end

M.minimise_focused = function(_window, _pane)
  -- Close the meta-session pane only. The per-agent zellij session keeps
  -- running (zj <branch> just detaches a client). Bring back with
  -- `agent restore` for all or `agent attach <name>` for one. consolidate
  -- reflows the remaining panes so the grid stays tidy.
  shell.run({ shell.fish(), "-c", "_agent_minimise_focused" })
end

M.attach_picker = function(window, pane)
  -- Spawn a temporary tab that runs the fzf session picker with live preview
  -- of each session's viewport. On pick, fires `agent attach <name>` detached
  -- so the picker tab can close immediately — otherwise the tab teardown
  -- races agent's mux operations and the new pane never lands in the
  -- meta-session.
  --
  -- After picking, find the matching worktree and run `agent` from inside
  -- it (agent.fish needs a git repo to resolve --repo). Run synchronously so
  -- the picker tab stays alive for agent's mux ops (refocus of the calling
  -- pane); a short sleep at the end lets the mux flush before the tab tears
  -- down.
  spawn_fish_tab(
    window,
    pane,
    "set -l picked (_agent_session_picker); test -n \"$picked\"; and set -l wt (_agent_worktree_path $picked); test -n \"$wt\"; and cd $wt; and agent attach $picked; sleep 0.5",
    true
  )
end

return M
