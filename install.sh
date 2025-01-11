#!/bin/bash

CONFIG_DIR="$HOME/.config"
BACKUP_DIR="$HOME/.oldconfig"

if [ -d "$CONFIG_DIR" ]; then
    echo "Backing up existing config to $BACKUP_DIR..."
    mv "$CONFIG_DIR" "$BACKUP_DIR"
fi

echo "Creating new config directory from dotfiles..."
git clone https://github.com/DanielGilchrist/dotfiles.git "$CONFIG_DIR"

echo "Installation complete!"
