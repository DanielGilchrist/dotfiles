---@meta

-- Plugins declare their config classes on their defaults tables, so every
-- field reads as required. Setup and open calls take any subset of them.

---@class Lazy
---@field setup fun(opts: Partial<LazyConfig>)

---@class snacks.lazygit
---@overload fun(opts?: Partial<snacks.lazygit.Config>): snacks.win

---@class snacks.gitbrowse
---@overload fun(opts?: Partial<snacks.gitbrowse.Config>)

---@class snacks.dashboard
---@overload fun(opts?: Partial<snacks.dashboard.Opts>): snacks.dashboard.Class

---@class snacks.input
---@overload fun(opts: Partial<snacks.input.Opts>, on_confirm: fun(value?: string)): snacks.win

---@class snacks.win
---@field win? integer
---@field buf? integer

---@alias snacks.picker.Pos { [1]: integer, [2]: integer }

-- Ruby LSP defines its own requests, which Client:request forwards as-is
-- (runtime/lua/vim/lsp/client.lua:726 never checks the method against the spec).
---@class vim.lsp.Client
---@field request fun(self: vim.lsp.Client, method: vim.lsp.protocol.Method.ClientToServer.Request | "rubyLsp/workspace/dependencies", params?: table, handler?: lsp.Handler, bufnr?: integer): boolean, integer?
