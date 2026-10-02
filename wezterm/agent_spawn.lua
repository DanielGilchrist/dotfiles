local wezterm = require("wezterm")
local agents_tab = require("agents_tab")
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
---Spawn a tab for something the user interacts with (an editor, a picker).
---Focus follows the new tab. Anything that merely runs to completion should
---use run_background instead so the user never leaves the tab they were on.
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

---Run a fish command with no tab at all and report its last line of output
---as a notification. Keeps the user on the tab they were on.
---@param window Window
---@param title string
---@param fish_cmd string
---@param pane? Pane when given, the command sees WEZTERM_PANE so agent.fish treats it as running inside wezterm
local function run_background(window, title, fish_cmd, pane)
  local args = { shell.fish(), "-c", fish_cmd }
  if pane then args = { "env", "WEZTERM_PANE=" .. tostring(pane:pane_id()), table.unpack(args) } end
  local ok, stdout, stderr = wezterm.run_child_process(args)
  local output = shell.trim(ok and stdout or (stderr ~= "" and stderr or stdout))
  local last_line = output:match("([^\n]*)$") or ""
  notify(window, title, last_line ~= "" and last_line or (ok and "done" or "failed"))
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
---@param with_prompt boolean
---@return string
local function build_spawn_cmd(repo_path, name, with_prompt)
  -- agent.fish opens nvim for the prompt when no -e/--seed/--no-prompt is
  -- given and the worktree doesn't yet exist. When :wq closes nvim, agent.fish
  -- finishes spawning into the meta-session and exits, taking the temporary
  -- tab with it. --no-focus so it doesn't drag focus back to that tab; the
  -- agents tab is focused instead once the pane is up. cd inside fish rather
  -- than trusting wezterm's --cwd (canonicalisation bug, wez/wezterm#4618).
  local flags = with_prompt and "" or " --no-prompt"
  return "cd " .. shell.quote(repo_path) .. "; and agent attach " .. shell.quote(name) .. flags
    .. " --no-focus; and _term_focus (_agent_meta_tab_pane)"
end

local SEED_WITHOUT = "No prompt (default) — start Claude bare"
local SEED_WITH = "Write a seed prompt in nvim"

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

          name_window:perform_action(wezterm.action.InputSelector({
            title = "Seed prompt for " .. name .. "?",
            choices = { { label = SEED_WITHOUT, id = "none" }, { label = SEED_WITH, id = "prompt" } },
            action = wezterm.action_callback(function(seed_window, seed_pane, seed_choice)
              if not seed_choice then return end
              if seed_choice == "prompt" then
                -- nvim needs a real tab; the command refocuses the agents tab on exit.
                spawn_fish_tab(seed_window, seed_pane, build_spawn_cmd(repo_path, name, true), true)
                return
              end
              run_background(seed_window, "agent attach", build_spawn_cmd(repo_path, name, false), seed_pane)
              agents_tab.focus(seed_window)
            end),
          }), target)
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
    action = wezterm.action_callback(function(inner_window, _, branch)
      if not branch then return end
      run_background(inner_window, "agent rm", "agent rm --force " .. shell.quote(branch))
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

M.remove_focused = function(window, _pane)
  local cwd = M.focused_worktree(window)
  if not cwd then return end
  local branch = path_utils.basename(cwd)
  if branch == "" then return end

  -- agent rm without --force refuses on dirty branches; the refusal arrives
  -- as the notification and the user can rerun with --force from a shell.
  run_background(window, "agent rm", "agent rm " .. shell.quote(branch))
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
