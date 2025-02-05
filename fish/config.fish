set CONFIG_DIR "$HOME/.config"

if test -f ~/.config/fish/alias.fish
  source ~/.config/fish/alias.fish
end

if test -d $CONFIG_DIR/fish/secret
  for f in $CONFIG_DIR/fish/secret/**/*.fish
    source $f
  end
end

source $CONFIG_DIR/fish/config/apple_silicon.fish
source $CONFIG_DIR/fish/config/intel.fish

if test -f "$HOME/.cargo/env.fish"
  source "$HOME/.cargo/env.fish"
end

set -gx MAKEFLAGS "-j$(nproc)"
set -gx XDG_CONFIG_HOME "$HOME/.config"
set -gx HOMEBREW_NO_AUTO_UPDATE 1
set -gx JAVA_HOME "/Applications/Android Studio.app/Contents/jbr/Contents/Home"
set -gx ANDROID_HOME $HOME/Library/Android/sdk
set -gx EDITOR (which nvim)

set -gx ASDF_CONFIG_FILE "$HOME/.config/.asdfrc"
set -gx ASDF_GOLANG_MOD_VERSION_ENABLED true

set -gx PATH $HOME/.asdf/shims $PATH
set -gx PATH $PATH $ANDROID_HOME/emulator
set -gx PATH $PATH $ANDROID_HOME/platform-tools

if status is-interactive
  # Commands to run in interactive sessions can go here
end
