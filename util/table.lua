local table_utils = {}

-- https://github.com/bew/dotfiles/blob/main/gui/wezterm/lib/mystdlib.lua#L4
function table_utils.merge_all(...)
  local ret = {}
  for _, tbl in ipairs({...}) do
    for k, v in pairs(tbl) do
      ret[k] = v
    end
  end
  return ret
end

return {
  table_utils = table_utils
}
