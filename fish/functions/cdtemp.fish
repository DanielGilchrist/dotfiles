
function cdtemp
  set -l argc (count $argv)
  set -l temp_dir "$HOME/Documents/temp"

  switch $argc
    case 0
      cd $temp_dir
    case 1
      cd "$temp_dir/$argv"
    case '*'
      echo "Error: Too many arguments. Expected 0 or 1, but got $argc" >&2
  end
end
