local config = require("agent.config")
local session = require("agent.session")
local zellij = require("agent.zellij")
local ui = require("agent.ui")

---@class Agent
---@field config AgentConfig
local M = {}
M.config = config

---@param msg string
---@param level integer|nil
local function notify(msg, level)
  vim.notify(msg, level or vim.log.levels.INFO, { title = "agent" })
end

---@param target string|nil
---@return string|nil
local function require_target(target)
  target = target or session.resolve()
  if not target then
    notify("no agent session resolved (cwd not in a worktree, no current branch)", vim.log.levels.WARN)
    return nil
  end
  if not zellij.session_exists(target) then
    notify("session not running: " .. target, vim.log.levels.WARN)
    return nil
  end
  return target
end

---@param target string|nil
---@param payload string
---@param submit boolean
local function send(target, payload, submit)
  local resolved = require_target(target)
  if not resolved then return end
  if not zellij.write_chars(resolved, payload) then
    notify("zellij write failed", vim.log.levels.ERROR)
    return
  end
  if submit then zellij.submit(resolved) end
end

---@param session_override string|nil
function M.send_file(session_override)
  local path = vim.api.nvim_buf_get_name(0)
  if path == "" then
    notify("buffer has no file path", vim.log.levels.WARN)
    return
  end
  send(session_override, "@" .. path, false)
end

---@param session_override string|nil
function M.send_visual(session_override)
  local saved = vim.fn.getreg('"')
  vim.cmd('noautocmd silent normal! "vy')
  local text = vim.fn.getreg("v")
  vim.fn.setreg('"', saved)
  if not text or text == "" then return end
  send(session_override, text, false)
end

function M.send_prompt()
  vim.ui.input({ prompt = "agent prompt: " }, function(input)
    if not input or input == "" then return end
    send(nil, input, true)
  end)
end

---@type {name: string|nil, term: any|nil}
M.active = { name = nil, term = nil }
---Last session we were attached to, kept across hide/show so toggle re-attaches the same one.
---@type string|nil
M.last_attached = nil

---@param term any
---@return boolean
local function term_alive(term)
  return term ~= nil and type(term.valid) == "function" and term:valid()
end

---Open a snacks terminal attached to the named zellij session, marking it active.
---@param name string
function M.attach_in_terminal(name)
  if not Snacks or not Snacks.terminal then
    notify("Snacks.terminal not available", vim.log.levels.ERROR)
    return
  end

  if M.active.name == name and term_alive(M.active.term) then
    M.active.term:focus()
    return
  end

  if term_alive(M.active.term) then
    pcall(function() M.active.term:hide() end)
  end

  local term = Snacks.terminal({ "zellij", "attach", name }, {
    win = { position = "right" },
    -- auto_close=false skips Snacks' TermClose handler (which is what fires the
    -- "exited with code -1" notification). We close the buffer ourselves on detach.
    auto_close = false,
  })
  M.active = { name = name, term = term }
  M.last_attached = name
end

---Close the active terminal. The shell wrapper around `zellij attach`
---traps signals and exits 0, so closing the buffer is clean.
local function detach_active()
  if not term_alive(M.active.term) then return end
  local term = M.active.term
  M.active = { name = nil, term = nil }
  pcall(function()
    if term.close then term:close() else term:hide() end
  end)
end

---Toggle the active session's terminal. Detaches on hide so zellij re-renders
---at the wezterm pane's full width while the nvim terminal is closed. Reopen
---re-attaches via Snacks.terminal.
function M.toggle()
  if term_alive(M.active.term) then
    detach_active()
    return
  end
  if M.last_attached and zellij.session_exists(M.last_attached) then
    M.attach_in_terminal(M.last_attached)
    return
  end
  M.open_or_pick()
end

function M.open_or_pick()
  local sessions = zellij.list_sessions()
  if #sessions == 0 then
    notify("no zellij sessions running — use <leader>zn to start one", vim.log.levels.WARN)
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

---Prompt for a session name, then open a multi-line prompt buffer; on submit, run the `agent` fish function.
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

          local attempts = 0
          local timer = vim.uv.new_timer()
          assert(timer, "could not create timer")
          timer:start(0, 200, vim.schedule_wrap(function()
            attempts = attempts + 1
            if zellij.session_exists(name) then
              timer:stop()
              timer:close()
              M.attach_in_terminal(name)
            elseif attempts > 25 then
              timer:stop()
              timer:close()
              notify("session " .. name .. " never came up", vim.log.levels.WARN)
            end
          end))
        end)
      end)
    end)
  end)
end

function M.switch_target()
  ui.pick_session({
    include_new = false,
    on_pick = function(name)
      if not name then return end
      vim.b.agent_session = name
      notify("buffer target: " .. name)
    end,
  })
end

function M.kill_target()
  local resolved = require_target(nil)
  if not resolved then return end
  vim.ui.select({ "yes", "no" }, { prompt = "kill " .. resolved .. "?" }, function(choice)
    if choice ~= "yes" then return end
    if zellij.kill(resolved) then
      notify("killed " .. resolved)
    else
      notify("kill failed", vim.log.levels.ERROR)
    end
  end)
end

return M
