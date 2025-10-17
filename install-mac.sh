#!/bin/bash

CONFIG_DIR="$HOME/.config"
BACKUP_DIR="$HOME/.oldconfig"
DOTFILES_REPO="https://github.com/DanielGilchrist/dotfiles.git"

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

install_brew_cask() {
  local cask="$1"
  local executable="${2:-$1}"
  if command_exists $executable; then
    echo "$executable is already installed."
  else
    echo "Installing $cask cask..."
    brew install --cask $cask
  fi
}

install_asdf_plugin() {
  local plugin="$1"
  local url="$2"
  if ! asdf plugin list | grep -q "^$plugin$"; then
    echo "Installing asdf $plugin plugin..."
    asdf plugin add $plugin $url
  else
    echo "asdf $plugin plugin already installed."
  fi
}

is_dotfiles_repo() {
  if [ -d "$CONFIG_DIR/.git" ]; then
    cd "$CONFIG_DIR"
    remote_url=$(git remote get-url origin 2>/dev/null)
    if [ "$remote_url" = $DOTFILES_REPO ]; then
      return 0
    fi
  fi
  return 1
}

clone_dotfiles() {
  echo "Creating new config directory from dotfiles..."
  git clone "$DOTFILES_REPO" "$CONFIG_DIR"
}

if ! command_exists brew; then
  echo "Installing Homebrew..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
else
  echo "Homebrew is already installed."
fi

if [ -f "/opt/homebrew/bin/brew" ]; then
  eval "$(/opt/homebrew/bin/brew shellenv)" # Apple Silicon
elif [ -f "/usr/local/bin/brew" ]; then
  eval "$(/usr/local/bin/brew shellenv)"    # Intel
else
  echo "Error: Homebrew installation not found"
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
  echo "Creating new config directory from dotfiles..."
  clone_dotfiles
fi

echo
echo "Installing packages..."

install_brew_package fish
install_brew_cask wezterm@nightly wezterm
install_brew_package neovim nvim
install_brew_package gh
install_brew_package asdf
install_brew_cask font-jetbrains-mono-nerd-font

install_brew_package fd
install_brew_package ripgrep rg
install_brew_package fzf
install_brew_package lazygit
install_brew_package crystalline
install_brew_package spotify_player
install_brew_package watchman
install_brew_package imagemagick magick
install_brew_package ghostscript gs
install_brew_package sst/tap/opencode opencode
install_brew_package libsixel
install_brew_package mpv

install_asdf_plugin ruby https://github.com/asdf-vm/asdf-ruby.git
install_asdf_plugin crystal https://github.com/asdf-community/asdf-crystal.git
install_asdf_plugin nodejs https://github.com/asdf-vm/asdf-nodejs.git
install_asdf_plugin golang https://github.com/asdf-community/asdf-golang.git

if ! command_exists rustup; then
  echo "Installing rustup..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  echo "Installing Rust..."
  rustup default stable
else
  echo "rustup is already installed."
fi

if ! command_exists youtube-tui; then
  echo "Installing youtube-tui..."
  export PKG_CONFIG_PATH="/opt/homebrew/lib/pkgconfig:$PKG_CONFIG_PATH"
  export LIBRARY_PATH="/opt/homebrew/lib:$LIBRARY_PATH"
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
