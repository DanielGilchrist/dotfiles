local function open_test()
  local ft = vim.bo.filetype

  if ft == "ruby" then
    local ruby = require("custom.test_open.ruby")
    ruby.open_test()
  else
    require("utils.notify").error("Unsupported filetype \"" .. ft .. "\" for opening tests!")
  end
end

return {
  open_test = open_test
}
