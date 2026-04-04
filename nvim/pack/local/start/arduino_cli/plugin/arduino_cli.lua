local arduino = function()
  return require("arduino_cli")
end

vim.api.nvim_create_user_command(
  "ArduinoCompile",
  function()
    arduino().compile()
  end,
  { desc = "Compile for configured board in local sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoUpload",
  function()
    arduino().upload()
  end,
  { desc = "Upload to configured board in local sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoRun",
  function()
    arduino().run()
  end,
  { desc = "Compile and upload to configured board in sketch.yml" }
)

vim.api.nvim_create_user_command(
  "ArduinoRunSelect",
  function()
    arduino().run_select()
  end,
  { desc = "Select board then compile and upload to it" }
)

vim.api.nvim_create_autocmd("FileType", {
  pattern = "arduino",
  callback = function(ev)
    local map = function(lhs, rhs, desc)
      vim.keymap.set("n", lhs, rhs, { buf = ev.buf, desc = desc })
    end

    map("<leader>rc", "<cmd>ArduinoCompile<cr>", "Arduino: Compile")
    map("<leader>ru", "<cmd>ArduinoUpload<cr>", "Arduino: Upload")
    map("<leader>rr", "<cmd>ArduinoRun<cr>", "Arduino: Run (Compile + Upload)")
    map("<leader>rs", "<cmd>ArduinoRunSelect<cr>", "Arduino: Select Board and Run")
  end,
})
