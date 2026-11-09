function _if_installed --description "Run a command if it is installed, otherwise do nothing"
  type -q $argv[1]; or return 0
  $argv
end
