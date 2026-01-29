#!/bin/bash

CONFIG_DIR="$HOME/.config"
BACKUP_DIR="$HOME/.oldconfig"
DOTFILES_REPO="https://github.com/DanielGilchrist/dotfiles.git"

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

install_package() {
  local package="$1"
  local executable="${2:-$1}"
  if command_exists "$executable"; then
    echo "$executable is already installed."
  else
    echo "Installing $package..."
    paru -S --noconfirm "$package"
  fi
}

install_asdf_plugin() {
  local plugin="$1"
  local url="$2"
  if ! asdf plugin list | grep -q "^$plugin$"; then
    echo "Installing asdf $plugin plugin..."
    asdf plugin add "$plugin" "$url"
  else
    echo "asdf $plugin plugin already installed."
  fi
}

is_dotfiles_repo() {
  if [ -d "$CONFIG_DIR/.git" ]; then
    cd "$CONFIG_DIR"
    remote_url=$(git remote get-url origin 2>/dev/null)
    if [ "$remote_url" = "$DOTFILES_REPO" ]; then
      return 0
    fi
  fi
  return 1
}

clone_dotfiles() {
  echo "Creating new config directory from dotfiles..."
  git clone "$DOTFILES_REPO" "$CONFIG_DIR"
}

if ! command_exists paru; then
  echo "paru is not installed. Please install paru first."
  echo "See: https://github.com/Morganamilo/paru"
  exit 1
fi

if is_dotfiles_repo; then
  echo "dotfiles repository already exists in $CONFIG_DIR, skipping clone..."
elif [ -d "$CONFIG_DIR" ]; then
  read -p "$CONFIG_DIR already exists. Would you like to back it up and continue? (y/n) " -n 1 -r
  echo
  if [[ $REPLY =~ ^[Yy]$ ]]; then
    echo "Backing up existing config to $BACKUP_DIR..."
    mv "$CONFIG_DIR" "$BACKUP_DIR"
    clone_dotfiles
  else
    echo "Installation cancelled."
    exit 1
  fi
else
  clone_dotfiles
fi

echo
echo "Installing packages..."

install_package fish
install_package wezterm
install_package neovim nvim
install_package wl-clipboard wl-copy
install_package github-cli gh
install_package asdf-vm asdf
install_package ttf-jetbrains-mono-nerd

install_package fd
install_package ripgrep rg
install_package fzf
install_package lazygit
install_package crystalline-bin crystalline
install_package spotify-player
install_package imagemagick magick
install_package ghostscript gs
install_package opencode
install_package libsixel
install_package mpv
install_package btop
install_package rainfrog
install_package arduino-cli

# watchman is currently broken on Arch - upstream hasn't provided Linux binaries since April 2024
# Uncomment if you want to try anyway:
# install_package watchman

install_asdf_plugin ruby https://github.com/asdf-vm/asdf-ruby.git
install_asdf_plugin crystal https://github.com/asdf-community/asdf-crystal.git

if ! command_exists rustup; then
  echo "Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  source "$HOME/.cargo/env"
  echo "Installing Rust stable..."
  rustup default stable
else
  echo "rustup is already installed."
fi

if ! command_exists youtube-tui; then
  echo "Installing youtube-tui..."
  cargo install youtube-tui
else
  echo "youtube-tui is already installed."
fi

if ! grep -q fish /etc/shells; then
  echo "Adding fish to /etc/shells..."
  echo "$(which fish)" | sudo tee -a /etc/shells
fi

if [ "$SHELL" != "$(which fish)" ]; then
  echo "Setting fish as default shell..."
  chsh -s "$(which fish)"
fi

echo
echo "Installation complete!"
echo "Start a new terminal session to use fish shell."
