-- Minimal Chrome DevTools Protocol client over `--remote-debugging-pipe`
-- (JSON messages NUL-terminated on fds 3 and 4), driven with libuv so a
-- single headless Chrome stays alive for the whole nvim session. One page
-- is kept loaded with marked.js and the GitHub stylesheet; renders swap
-- the markdown in place and screenshots clip the wanted slice.

local is = require("utils.is")

---@class MdLayout
---@field height integer content height in CSS px
---@field headings integer[] top of each heading in CSS px

---@class MdChrome
---@field binary string
---@field template string path of the HTML page to keep loaded
---@field handle? uv.uv_process_t
---@field to_chrome? uv.uv_pipe_t
---@field from_chrome? uv.uv_pipe_t
---@field session_id? string
---@field buffer string
---@field next_id integer
---@field callbacks table<integer, fun(result: table|nil, err: table|nil)>
---@field ready boolean
---@field waiting fun()[] callbacks queued until the page is ready
---@field on_exit? fun(code: integer)
local Chrome = {}
Chrome.__index = Chrome

local uv = vim.uv

local CHROME_CANDIDATES = {
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Chromium.app/Contents/MacOS/Chromium",
  "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
  "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
}

---@return string|nil
function Chrome.binary()
  for _, candidate in ipairs(CHROME_CANDIDATES) do
    if is.executable(candidate) then return candidate end
  end
  return nil
end

---@param binary string
---@param template string
---@return MdChrome
function Chrome.new(binary, template)
  return setmetatable({
    binary = binary,
    template = template,
    buffer = "",
    next_id = 0,
    callbacks = {},
    ready = false,
    waiting = {},
  }, Chrome)
end

---@return boolean
function Chrome:alive()
  return self.handle ~= nil and self.handle:is_active()
end

---@param method string
---@param params? table
---@param cb? fun(result: table|nil, err: table|nil)
---@param browser_level? boolean send without the page session (Target.*, Browser.*)
function Chrome:send(method, params, cb, browser_level)
  if not self.to_chrome then return end
  self.next_id = self.next_id + 1
  if cb then self.callbacks[self.next_id] = cb end
  local message = { id = self.next_id, method = method, params = params or vim.empty_dict() }
  if not browser_level and self.session_id then message.sessionId = self.session_id end
  self.to_chrome:write(vim.json.encode(message) .. "\0")
end

---@param chunk string
function Chrome:consume(chunk)
  self.buffer = self.buffer .. chunk
  while true do
    local nul = self.buffer:find("\0", 1, true)
    if not nul then return end
    local raw = self.buffer:sub(1, nul - 1)
    self.buffer = self.buffer:sub(nul + 1)
    local ok, message = pcall(vim.json.decode, raw)
    if ok and message.id and self.callbacks[message.id] then
      local cb = self.callbacks[message.id]
      self.callbacks[message.id] = nil
      vim.schedule(function() cb(message.result, message.error) end)
    end
  end
end

---Run <cb> once the page is loaded and marked.js is available.
---@param cb fun()
function Chrome:when_ready(cb)
  if self.ready then return cb() end
  table.insert(self.waiting, cb)
end

function Chrome:flush_waiting()
  self.ready = true
  local waiting = self.waiting
  self.waiting = {}
  for _, cb in ipairs(waiting) do cb() end
end

---Spawn Chrome, open one page on the template and wait for marked.js.
---@param on_error fun(message: string)
function Chrome:start(on_error)
  self.to_chrome = uv.new_pipe(false)
  self.from_chrome = uv.new_pipe(false)

  self.handle = uv.spawn(self.binary, {
    args = {
      "--headless=new",
      "--remote-debugging-pipe",
      "--disable-gpu",
      "--no-first-run",
      "--hide-scrollbars",
      "about:blank",
    },
    stdio = { nil, nil, nil, self.to_chrome, self.from_chrome },
  }, function(code)
    self.handle = nil
    self.ready = false
    if self.on_exit then vim.schedule(function() self.on_exit(code) end) end
  end)

  if not self.handle then
    return on_error("could not spawn " .. self.binary)
  end

  self.from_chrome:read_start(function(err, chunk)
    if err or not chunk then return end
    self:consume(chunk)
  end)

  self:send("Target.createTarget", { url = "about:blank" }, function(created, err)
    if not created then return on_error("Target.createTarget failed: " .. vim.inspect(err)) end
    self:send("Target.attachToTarget", { targetId = created.targetId, flatten = true }, function(attached, attach_err)
      if not attached then return on_error("Target.attachToTarget failed: " .. vim.inspect(attach_err)) end
      self.session_id = attached.sessionId
      self:send("Page.navigate", { url = "file://" .. self.template }, function(_, nav_err)
        if nav_err then return on_error("Page.navigate failed: " .. vim.inspect(nav_err)) end
        self:send("Runtime.evaluate", {
          expression = "new Promise(resolve => { const poll = () => window.__render ? resolve(true) : setTimeout(poll, 10); poll(); })",
          awaitPromise = true,
          returnByValue = true,
        }, function(_, eval_err)
          if eval_err then return on_error("template did not load: " .. vim.inspect(eval_err)) end
          self:flush_waiting()
        end)
      end)
    end, true)
  end, true)
end

---Size the emulated viewport to the preview window so a plain screenshot
---is exactly the visible slice; painting stays bounded by the viewport
---rather than the whole document.
---@param css_width integer
---@param css_height integer
---@param scale integer
---@param cb fun(err: string|nil)
function Chrome:set_viewport(css_width, css_height, scale, cb)
  self:send("Emulation.setDeviceMetricsOverride", {
    width = css_width,
    height = css_height,
    deviceScaleFactor = scale,
    mobile = false,
  }, function(_, err)
    if err then return cb(vim.inspect(err)) end
    self:send("Runtime.evaluate", { expression = ("window.__setWidth(%d)"):format(css_width) }, function(_, eval_err)
      cb(eval_err and vim.inspect(eval_err) or nil)
    end)
  end)
end

---Replace the rendered markdown and return the resulting layout.
---@param markdown string
---@param cb fun(layout: MdLayout|nil, err: string|nil)
function Chrome:set_content(markdown, cb)
  local encoded = vim.json.encode(markdown)
  self:send("Runtime.evaluate", {
    expression = "window.__render(" .. encoded .. ")",
    awaitPromise = true,
    returnByValue = true,
  }, function(result, err)
    if err or not result or not result.result or not result.result.value then
      return cb(nil, vim.inspect(err or result))
    end
    local ok, layout = pcall(vim.json.decode, result.result.value)
    if not ok or not is.table(layout) then return cb(nil, "bad layout: " .. tostring(result.result.value)) end
    cb(layout)
  end)
end

---Scroll the page to <y> and screenshot the viewport.
---@param y integer CSS px from the top
---@param cb fun(png: string|nil, err: string|nil) decoded PNG bytes
function Chrome:capture(y, cb)
  self:send("Runtime.evaluate", { expression = ("window.scrollTo(0, %d)"):format(y) }, function()
    self:send("Page.captureScreenshot", { format = "png", optimizeForSpeed = true }, function(result, err)
      if err or not result or not result.data then return cb(nil, vim.inspect(err or result)) end
      cb(vim.base64.decode(result.data))
    end)
  end)
end

function Chrome:stop()
  self.on_exit = nil
  if self.handle then
    self:send("Browser.close", nil, nil, true)
    local handle = self.handle
    uv.new_timer():start(500, 0, function()
      if handle:is_active() then handle:kill("sigkill") end
    end)
  end
  if self.to_chrome then self.to_chrome:close() end
  if self.from_chrome then self.from_chrome:close() end
  self.to_chrome, self.from_chrome, self.handle = nil, nil, nil
  self.ready = false
end

return Chrome
