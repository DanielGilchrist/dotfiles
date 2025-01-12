# Apple Silicon
if test -f /opt/homebrew/bin/brew
    eval "$(/opt/homebrew/bin/brew shellenv)"
end

# Intel
if test -f /usr/local/bin/brew
    eval "$(/usr/local/bin/brew shellenv)"
end

# Apple Silicon
if test -f /opt/homebrew/opt/asdf/libexec/asdf.fish
  source /opt/homebrew/opt/asdf/libexec/asdf.fish
end

# Intel
if test -f /usr/local/opt/asdf/libexec/asdf.fish
  source /usr/local/opt/asdf/libexec/asdf.fish
end

if test -d ~/.config/fish/secret
  for f in ~/.config/fish/secret/**/*.fish
    source $f
  end
end

if test -f ~/.config/fish/alias.fish
  source ~/.config/fish/alias.fish
end

if test -f "$HOME/.cargo/env.fish"
  source "$HOME/.cargo/env.fish"
end

set -gx HOMEBREW_NO_AUTO_UPDATE 1
set -gx JAVA_HOME "/Applications/Android Studio.app/Contents/jbr/Contents/Home"
set -gx ANDROID_HOME $HOME/Library/Android/sdk
set -gx EDITOR (which nvim)  # Use `which` to find the correct nvim path

set -gx PATH $HOME/.asdf/shims $PATH
set -gx PATH $PATH $ANDROID_HOME/emulator
set -gx PATH $PATH $ANDROID_HOME/platform-tools

if status is-interactive
  # Commands to run in interactive sessions can go here
end
