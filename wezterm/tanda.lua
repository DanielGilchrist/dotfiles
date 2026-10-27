local wezterm = require("wezterm")
local notify = require("utils.notify")
local shell = require("utils.shell")

---@class TandaModule
---@field palette_entries fun(): table[]
local M = {}

---@class TandaClockAction
---@field brief string
---@field args string[]

---@type TandaClockAction[]
local ACTIONS = {
  { brief = "Tanda: Clock in", args = { "clockin", "start" } },
  { brief = "Tanda: Start break", args = { "clockin", "break", "start" } },
  { brief = "Tanda: Finish break", args = { "clockin", "break", "finish" } },
  { brief = "Tanda: Clock out", args = { "clockin", "finish" } },
}

---Run the CLI with no tab and report its output as a notification. Goes
---through a login fish so the `tanda_cli` alias resolves as it does in a shell.
---@param window Window
---@param action TandaClockAction
local function run(window, action)
  local fish_cmd = "tanda_cli --no-colour " .. table.concat(action.args, " ")
  local ok, stdout, stderr = wezterm.run_child_process({ shell.fish(), "-l", "-c", fish_cmd })
  local output = shell.trim(ok and stdout or (stderr ~= "" and stderr or stdout))
  local summary = output:gsub("\n", " · ")
  notify(window, action.brief, summary ~= "" and summary or (ok and "done" or "failed"))
end

---Command palette entries, one per clock action.
---@return table[]
M.palette_entries = function()
  local entries = {}
  for _, action in ipairs(ACTIONS) do
    table.insert(entries, {
      brief = action.brief,
      action = wezterm.action_callback(function(window) run(window, action) end),
    })
  end
  return entries
end

return M
