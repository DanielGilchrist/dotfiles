local config = require("agent.config")
local session = require("agent.session")
local zellij = require("agent.zellij")
local ui = require("agent.ui")
local composer = require("agent.composer")

---@class Agent
---@field config AgentConfig
local M = {}
M.config = config

---Map of tab page id → agent name. An agent owns one tab page; jumping to
---that page is the "attach" UX and closing it is the "detach" UX. Cleared
---on TabClosed so it can't go stale.
---@type table<integer, string>
M.tab_agents = {}
---Most recently focused agent, used by `<C-.>` toggle and as a fallback
---send target from non-agent tabs.
---@type string|nil
M.last_attached = nil

---@param msg string
---@param level integer|nil
local function notify(msg, level)
  vim.notify(msg, level or vim.log.levels.INFO, { title = "agent" })
end

---@param name string
---@return integer|nil tabid
local function tab_for_agent(name)
  for tabid, n in pairs(M.tab_agents) do
    if n == name and vim.api.nvim_tabpage_is_valid(tabid) then return tabid end
  end
  return nil
end

---Agent owning the current tab page (nil for the main / non-agent tab).
---@return string|nil
local function current_agent()
  return M.tab_agents[vim.api.nvim_get_current_tabpage()]
end

---Resolve which agent a send-style action targets. Priority: explicit arg →
---agent owning the current tab → most-recently focused agent.
---@param target string|nil
---@return string|nil
local function require_target(target)
  target = target or current_agent() or M.last_attached
  if not target then
    notify("no agent to send to — attach one with <leader>ao or spawn with <leader>an", vim.log.levels.WARN)
    return nil
  end
  if not zellij.session_exists(target) then
    notify("session not running: " .. target, vim.log.levels.WARN)
    return nil
  end
  return target
end

---Send arbitrary text to the active agent. Returns true if delivered.
---@param text string
---@param submit boolean
---@return boolean
function M.send_text(text, submit)
  local resolved = require_target(nil)
  if not resolved then return false end
  if not zellij.write_chars(resolved, text) then
    notify("zellij write failed", vim.log.levels.ERROR)
    return false
  end
  if submit then zellij.submit(resolved) end
  return true
end

---Open the composer with a `@<abs>:<l1>-<l2>` reference to the visual
---selection pre-filled. Saves the buffer first so claude reads the same
---content we're pointing at.
function M.send_visual()
  local path = vim.api.nvim_buf_get_name(0)
  if path == "" then
    notify("buffer has no file path", vim.log.levels.WARN)
    return
  end
  local s = vim.api.nvim_buf_get_mark(0, "<")
  local e = vim.api.nvim_buf_get_mark(0, ">")
  if s[1] == 0 or e[1] == 0 then
    notify("no visual selection", vim.log.levels.WARN)
    return
  end
  if vim.bo.modified then pcall(vim.cmd, "silent! update") end
  composer.append(("@%s:%d-%d"):format(path, s[1], e[1]))
  composer.show()
end

function M.send_prompt()
  composer.show()
end

function M.toggle_composer()
  composer.toggle()
end

---Build the zellij command. When `initial_cmd` is supplied and the session
---doesn't yet exist, `zj` creates it with that command as the first pane;
---otherwise we just attach.
---@param name string
---@param cwd string|nil
---@param initial_cmd string|nil
---@return string[]
local function build_attach_cmd(name, cwd, initial_cmd)
  if not cwd and not initial_cmd then return { "zellij", "attach", name } end
  local fish_cmd = ""
  if cwd then fish_cmd = "cd " .. vim.fn.shellescape(cwd) .. "; " end
  fish_cmd = fish_cmd .. "zj " .. vim.fn.shellescape(name)
  if initial_cmd then fish_cmd = fish_cmd .. " -- " .. initial_cmd end
  return { "fish", "-c", fish_cmd }
end

---Open a tab page for the named agent: tab-local cwd, dashboard on the left,
---zellij attach terminal on the right. Idempotent — if a tab already exists
---for this agent, jump to it.
---@param name string
---@param opts? {cwd?: string, initial_cmd?: string}
function M.attach_in_terminal(name, opts)
  if not Snacks or not Snacks.terminal then
    notify("Snacks.terminal not available", vim.log.levels.ERROR)
    return
  end
  opts = opts or {}

  local existing = tab_for_agent(name)
  if existing then
    vim.api.nvim_set_current_tabpage(existing)
    M.last_attached = name
    return
  end

  local cwd = opts.cwd or session.session_cwd(name)

  -- If the agent's cwd matches the current cwd (or there's no cwd to swap
  -- to), the agent belongs *here* — open it as a side split in the current
  -- tab rather than a new tab page. This is the repo-session case.
  local current_cwd = vim.fn.getcwd()
  local needs_new_tab = cwd ~= nil and cwd ~= current_cwd

  local cmd = build_attach_cmd(name, opts.cwd, opts.initial_cmd)

  if not needs_new_tab then
    M.last_attached = name
    Snacks.terminal(cmd, { win = { position = "right" }, auto_close = false })
    vim.schedule(function()
      vim.cmd("wincmd h")
      vim.cmd("stopinsert")
    end)
    return
  end

  vim.cmd("tabnew")
  local tabid = vim.api.nvim_get_current_tabpage()
  vim.cmd("tcd " .. vim.fn.fnameescape(cwd))
  vim.t.tabname = name
  M.tab_agents[tabid] = name
  M.last_attached = name

  if Snacks.dashboard then
    pcall(Snacks.dashboard, {
      buf = vim.api.nvim_get_current_buf(),
      win = vim.api.nvim_get_current_win(),
    })
  end

  Snacks.terminal(cmd, { win = { position = "right" }, auto_close = false })
  vim.schedule(function()
    vim.cmd("wincmd h")
    vim.cmd("stopinsert")
  end)
end

---Toggle between the agent tab and wherever you were. If you're in an
---agent tab, jump back. If you're not and you have a last-attached agent
---with a live tab, jump to it. Otherwise fall through to the picker.
function M.toggle()
  if current_agent() then
    vim.cmd("tabprevious")
    return
  end
  if M.last_attached then
    local tab = tab_for_agent(M.last_attached)
    if tab then
      vim.api.nvim_set_current_tabpage(tab)
      return
    end
    if zellij.session_exists(M.last_attached) then
      M.attach_in_terminal(M.last_attached)
      return
    end
  end
  M.open_or_pick()
end

function M.open_or_pick()
  local sessions = zellij.list_sessions()
  if #sessions == 0 then
    notify("no zellij sessions running — <leader>an for a worktree agent, <leader>as for a repo session", vim.log.levels.WARN)
    return
  end

  local resolved = session.resolve()
  if resolved and zellij.session_exists(resolved) then
    M.attach_in_terminal(resolved)
    return
  end

  ui.pick_session({
    include_new = true,
    on_pick = function(name, is_new)
      if is_new then
        M.new_agent()
      elseif name then
        M.attach_in_terminal(name)
      end
    end,
  })
end

---Spawn a worktree agent: prompts for a kebab-case name, opens a multi-line
---prompt buffer for the seed, then runs `agent <name> --seed <tmp>` which
---creates `~/worktrees/<repo>/<name>` and starts Claude in a zellij session.
function M.new_agent()
  vim.ui.input({ prompt = "agent name (kebab-case): " }, function(name)
    if not name or vim.trim(name) == "" then return end
    name = vim.trim(name)

    ui.new_prompt(function(text)
      local tmp = vim.fn.tempname() .. ".md"
      local fd = io.open(tmp, "w")
      if not fd then
        notify("could not write seed file", vim.log.levels.ERROR)
        return
      end
      fd:write(text)
      fd:close()

      local repo = session.repo_root()
      ---@type string[]
      local cmd = {
        "fish", "-c",
        "agent " .. vim.fn.shellescape(name)
          .. " --seed " .. vim.fn.shellescape(tmp)
          .. (repo and (" --repo " .. vim.fn.shellescape(repo)) or ""),
      }

      vim.system(cmd, { text = true }, function(out)
        vim.schedule(function()
          if out.code ~= 0 then
            notify("agent spawn failed: " .. (out.stderr or ""), vim.log.levels.ERROR)
            return
          end
          notify(vim.trim(out.stdout or "spawned"))

          -- Trigger the pin-to-zero lua handler. Fish would do this via
          -- printf to /dev/tty, but nvim's vim.system subprocess doesn't
          -- have the right /dev/tty, so we emit the OSC from nvim itself —
          -- nvim's stdout is the wezterm pane's tty, so wezterm picks it up.
          local b64 = vim.fn.system({ "base64" }, "pin-agents-tab")
          io.write("\27]1337;SetUserVar=agent-action=" .. vim.trim(b64) .. "\7")
          io.flush()

          -- Poll for up to 30s — the per-agent session is created lazily by
          -- the meta-session pane's `zj` command, which can take a while.
          local attempts = 0
          local max_attempts = 150
          local timer = vim.uv.new_timer()
          assert(timer, "could not create timer")
          timer:start(0, 200, vim.schedule_wrap(function()
            attempts = attempts + 1
            if zellij.session_exists(name) then
              timer:stop()
              timer:close()
              M.attach_in_terminal(name)
            elseif attempts > max_attempts then
              timer:stop()
              timer:close()
              notify("session " .. name .. " never came up — check `zj` and the agents tab", vim.log.levels.WARN)
            end
          end))
        end)
      end)
    end)
  end)
end

---Spawn (or attach to) a claude session rooted at the current repo. No
---worktree, no prompts — session name defaults to the repo basename so
---it's idempotent and discoverable from mobile.
function M.new_repo_session()
  local repo = session.repo_root()
  if not repo then
    notify("not in a git repo", vim.log.levels.WARN)
    return
  end
  local name = vim.fn.fnamemodify(repo, ":t")
  if name == "" then
    notify("could not derive session name from repo path", vim.log.levels.ERROR)
    return
  end
  if zellij.session_exists(name) then
    M.attach_in_terminal(name, { cwd = repo })
    return
  end
  M.attach_in_terminal(name, { cwd = repo, initial_cmd = "claude" })
end

---Run `agent-rm --force <name>` to tear down worktree + session + branch
---and close the owning tab page if we have one.
---@param name string
local function tear_down(name)
  local cmd = { "fish", "-c", "agent-rm --force " .. vim.fn.shellescape(name) }
  vim.system(cmd, { text = true }, function(out)
    vim.schedule(function()
      if out.code ~= 0 then
        notify("kill failed: " .. (out.stderr or out.stdout or ""), vim.log.levels.ERROR)
        return
      end
      notify(vim.trim(out.stdout or ("killed " .. name)))
      local tab = tab_for_agent(name)
      if tab then pcall(vim.api.nvim_command, ("tabclose " .. vim.api.nvim_tabpage_get_number(tab))) end
      M.tab_agents[tab or 0] = nil
      if M.last_attached == name then M.last_attached = nil end
    end)
  end)
end

function M.kill_agent()
  local sessions = zellij.list_sessions()
  if #sessions == 0 then
    notify("no agent sessions to kill", vim.log.levels.WARN)
    return
  end

  -- Defer the picker open so we're not still inside the keymap callback when
  -- Snacks tries to set up its UI. Avoids E21 when the current buffer is
  -- non-modifiable (e.g. the dashboard).
  vim.schedule(function()
    ui.pick_kill({
      current = current_agent() or session.resolve(),
      on_pick = function(name)
        if not name then return end
        vim.ui.select({ "yes", "no" }, { prompt = "kill " .. name .. " (worktree + session + branch)?" }, function(confirm)
          if confirm ~= "yes" then return end
          tear_down(name)
        end)
      end,
    })
  end)
end

vim.api.nvim_create_autocmd("TabClosed", {
  group = vim.api.nvim_create_augroup("agent_nvim_tabs", { clear = true }),
  callback = function(args)
    local tabid = tonumber(args.file) -- nvim 0.10+ passes the closed tab number
    if tabid then M.tab_agents[tabid] = nil end
    -- belt-and-suspenders: drop any entries for tabs that no longer exist
    for id in pairs(M.tab_agents) do
      if not vim.api.nvim_tabpage_is_valid(id) then M.tab_agents[id] = nil end
    end
  end,
})

return M
