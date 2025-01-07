#!/bin/bash

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

install() {
  local package="$1"
  local executable="${2:-$1}"

  if command_exists $executable; then
    echo "$executable is already installed."
  else
    echo "Installing $package..."
    brew install $package
  fi
}

install fd
install ripgrep rg
install fzf
install lazygit
install crystalline
install spotify_player

echo -e "\nInstallation complete!"
