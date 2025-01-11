#!/bin/bash

CONFIG_DIR="$HOME/.config"
BACKUP_DIR="$HOME/.oldconfig"

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

install_brew_package() {
  local package="$1"
  local executable="${2:-$1}"
  if command_exists $executable; then
    echo "$executable is already installed."
  else
    echo "Installing $package..."
    brew install $package
  fi
}

if ! command_exists brew; then
  echo "Installing Homebrew..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
else
  echo "Homebrew is already installed."
fi

eval "$(/opt/homebrew/bin/brew shellenv)"

# Backup existing config if it exists
if [ -d "$CONFIG_DIR" ]; then
    echo "Backing up existing config to $BACKUP_DIR..."
    mv "$CONFIG_DIR" "$BACKUP_DIR"
fi

echo "Creating new config directory from dotfiles..."
git clone https://github.com/DanielGilchrist/dotfiles.git "$CONFIG_DIR"

echo "Installing packages..."

install_brew_package fish
install_brew_package wezterm
install_brew_package neovim
install_brew_package gh
install_brew_package asdf

# nvim specific
install_brew_package fd
install_brew_package ripgrep rg
install_brew_package fzf
install_brew_package lazygit
install_brew_package crystalline
install_brew_package spotify_player

if ! command_exists rustup; then
  echo "Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
else
  echo "rustup is already installed."
fi

if ! grep -q fish /etc/shells; then
  echo "Adding fish to /etc/shells..."
  echo "$(which fish)" | sudo tee -a /etc/shells
fi

if [ "$SHELL" != "$(which fish)" ]; then
  echo "Setting fish as default shell..."
  chsh -s "$(which fish)"
fi

echo "Installation complete!"
echo "Start a new terminal session to use fish shell."
