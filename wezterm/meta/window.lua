---@meta

-- wezterm-types declares these as never nil, but WezTerm returns an Option:
-- https://github.com/wezterm/wezterm/blob/main/wezterm-gui/src/scripting/guiwin.rs
-- https://github.com/wezterm/wezterm/blob/main/lua-api-crates/mux/src/window.rs
-- https://github.com/wezterm/wezterm/blob/main/lua-api-crates/mux/src/tab.rs
-- https://github.com/wezterm/wezterm/blob/main/lua-api-crates/mux/src/pane.rs
-- https://github.com/wezterm/wezterm/blob/main/wezterm-gui/src/termwindow/mod.rs (TabInformation)

---@class Window
local Window = {}

---@return MuxTab?
function Window:active_tab() end

---@return Pane?
function Window:active_pane() end

---@class MuxWindow
local MuxWindow = {}

---@return MuxTab?
function MuxWindow:active_tab() end

---@return Pane?
function MuxWindow:active_pane() end

---@class MuxTab
local MuxTab = {}

---@return Pane?
function MuxTab:active_pane() end

---@return MuxWindow?
function MuxTab:window() end

---@class Pane
local Pane = {}

---@return MuxWindow?
function Pane:window() end

---@class TabInformation
---@field active_pane PaneInformation?
