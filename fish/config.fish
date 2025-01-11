# ASDF setup
if test -f /opt/homebrew/opt/asdf/libexec/asdf.fish
    source /opt/homebrew/opt/asdf/libexec/asdf.fish  # Apple Silicon
else if test -f /usr/local/opt/asdf/libexec/asdf.fish
    source /usr/local/opt/asdf/libexec/asdf.fish     # Intel
end

# Secret configs
if test -d ~/.config/fish/secret
    for f in ~/.config/fish/secret/**/*.fish
        source $f
    end
end

# Source aliases
if test -f ~/.config/fish/alias.fish
    source ~/.config/fish/alias.fish
end

# Cargo setup
if test -f "$HOME/.cargo/env.fish"
    source "$HOME/.cargo/env.fish"
end

# Environment variables
set -gx HOMEBREW_NO_AUTO_UPDATE 1
set -gx JAVA_HOME "/Applications/Android Studio.app/Contents/jbr/Contents/Home"
set -gx ANDROID_HOME $HOME/Library/Android/sdk
set -gx EDITOR (which nvim)  # Use `which` to find the correct nvim path

# PATH modifications
set -gx PATH $HOME/.asdf/shims $PATH
set -gx PATH $PATH $ANDROID_HOME/emulator
set -gx PATH $PATH $ANDROID_HOME/platform-tools

if status is-interactive
    # Commands to run in interactive sessions can go here
end

eval "$(/opt/homebrew/bin/brew shellenv)"
