function oil
  nvim -c 'lua
    local oil = require("oil")
    oil.open()

    -- Override the close action to also quit nvim when oil closes
    local original_close = oil.close
    oil.close = function(...)
      original_close(...)
      vim.schedule(function() vim.cmd("quit") end)
    end'
end
