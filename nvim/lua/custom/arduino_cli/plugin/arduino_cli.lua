local arduino = function()
  return require("custom.arduino_cli")
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

vim.keymap.set("n", "<leader>rc", "<cmd>ArduinoCompile<cr>", { desc = "Arduino: Compile" })
vim.keymap.set("n", "<leader>ru", "<cmd>ArduinoUpload<cr>", { desc = "Arduino: Upload" })
vim.keymap.set("n", "<leader>rr", "<cmd>ArduinoRun<cr>", { desc = "Arduino: Run (Compile + Upload)" })
vim.keymap.set("n", "<leader>rs", "<cmd>ArduinoRunSelect<cr>", { desc = "Arduino: Select Board and Run" })
