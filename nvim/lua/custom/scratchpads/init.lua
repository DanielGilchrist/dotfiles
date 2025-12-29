local notify = require("utils.notify")
local is = require("utils.is")
local str = require("utils.str")
local ui = require("utils.ui")
local scratchpads_dir = vim.fn.expand("~/.local/share/scratchpads/")

local function scratch_search(_title, opts)
  opts = opts == nil and {} or opts

  local args = {
    dirs = { scratchpads_dir },
    hidden = false,
  }

  if opts.confirm then
    args.confirm = function(picker, item)
      picker:close()

      if opts.multiselect then
        opts.confirm(picker:selected())
      else
        opts.confirm(item)
      end
    end
  end

  Snacks.picker.files(args)
end

local function build_new_filename(count, extension)
  return scratchpads_dir .. "scratch" .. count .. extension
end

local function scratchpads_dir_not_created()
  return is.not_directory(scratchpads_dir)
end

local function create_scratchpad(extension)
  if scratchpads_dir_not_created() then
    vim.fn.mkdir(scratchpads_dir, "p")
  end

  local count = 1
  local filename = build_new_filename(count, extension)

  while is.file_readable(filename) do
    count = count + 1
    filename = build_new_filename(count, extension)
  end

  vim.cmd("edit " .. filename)
end

local function new_scratchpad()
  vim.ui.select({
    { lang = "Ruby",       ext = ".rb" },
    { lang = "Crystal",    ext = ".cr" },
    { lang = "JavaScript", ext = ".js" },
    { lang = "SQL",        ext = ".sql" },
    { lang = "Text",       ext = ".txt" },
    { lang = "Bash",       ext = ".sh" },
    { lang = "Fish",       ext = ".fish" },
    { lang = "JSON",       ext = ".json" },
    { lang = "JSONC",      ext = ".jsonc" }
  }, {
    prompt = "Select a language",
    format_item = function(item)
      return item.lang
    end,
  }, function(choice)
    if choice then
      create_scratchpad(choice.ext)
    end
  end)
end

local function open_scratchpad()
  if scratchpads_dir_not_created() or #vim.fn.globpath(scratchpads_dir, "*") == 0 then
    notify.warn("No scratchpads have been created. Create one with `:ScratchNew`.")
  else
    scratch_search("Search Scratchpads")
  end
end

local function valid_scratch_file(filename)
  return is.not_empty(filename) and is.file_readable(filename) and str.includes(filename, scratchpads_dir, true)
end

local function rename_scratchpad()
  local old_filename = vim.api.nvim_buf_get_name(0)

  if not valid_scratch_file(old_filename) then
    notify.error(old_filename .. " is not a scratch file!")
    return
  end

  vim.ui.input({
    prompt = "New name: ",
    default = vim.fn.fnamemodify(old_filename, ":t"),
  }, function(new_name)
    if new_name then
      local new_filename = scratchpads_dir .. new_name

      vim.fn.rename(old_filename, new_filename)
      vim.cmd("bd!")
      vim.cmd("edit " .. new_filename)
      notify.info("Scratchpad renamed from " .. old_filename .. " to " .. new_filename)
    end
  end)
end

local function remove_scratchpad()
  local function delete_scratchpads(selected)
    if not selected or #selected == 0 then
      notify.warn("No scratchpads selected for removal!")
      return
    end

    local files_to_delete = {}
    for i, file in ipairs(selected) do
      files_to_delete[i] = file.text
    end

    ui.confirm(
      "Remove selected scratchpads?",
      function(choice)
        if choice == "Ok" then
          for _, file in ipairs(files_to_delete) do
            local success, err = os.remove(file)
            if not success then
              notify.error(string.format("Failed to remove %s: %s", file, err))
            else
              notify.info(string.format("Removed %s", file))
            end
          end
        else
          notify.warn("Action to remove scratchpads cancelled.")
        end
      end
    )
  end

  scratch_search("Delete Scratchpad", {
    multiselect = true,
    confirm = delete_scratchpads,
  })
end

return {
  new = new_scratchpad,
  open = open_scratchpad,
  rename = rename_scratchpad,
  remove = remove_scratchpad,
}
