function cdc
  set -l argc (count $argv)

  switch $argc
    case 0
      cd $CONFIG_DIR
    case 1
      cd "$CONFIG_DIR/$argv"
    case '*'
      echo "Error: Too many arguments. Expected 0 or 1, but got $argc" >&2
  end
end
