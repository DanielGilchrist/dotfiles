-- vim.pack hooks (must be defined before any vim.pack.add() calls)
vim.api.nvim_create_autocmd("PackChanged", {
  callback = function(ev)
    local name = ev.data.spec.name
    local kind = ev.data.kind
    if kind ~= "install" and kind ~= "update" then return end

    if name == "nvim-treesitter" then
      if not ev.data.active then vim.cmd.packadd("nvim-treesitter") end
      vim.cmd("TSUpdate")
    elseif name == "blink.cmp" then
      vim.system({ "cargo", "build", "--release" }, { cwd = ev.data.path }):wait()
    end
  end,
})
