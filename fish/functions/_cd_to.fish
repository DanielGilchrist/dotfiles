function _cd_to
  set -l path $argv[1]
  set -l args $argv[2..-1]
  set -l argc (count $args)

  switch $argc
    case 0
      cd $path
    case 1
      cd "$path/$args"
    case "*"
      echo "Error: Too many arguments. Expected 0 or 1, but got $argc" >&2
  end
end
