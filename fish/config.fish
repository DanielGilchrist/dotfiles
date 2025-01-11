set -gx PATH /Users/danielgilchrist/.asdf/shims $PATH
source /usr/local/opt/asdf/libexec/asdf.fish

if test -d ~/.config/fish/secret
  for f in ~/.config/fish/secret/**/*.fish
    source $f
  end
end

. ~/.config/fish/alias.fish

if status is-interactive
    # Commands to run in interactive sessions can go here
end

set -gx HOMEBREW_NO_AUTO_UPDATE 1

source "$HOME/.cargo/env.fish"

set -gx JAVA_HOME "/Applications/Android Studio.app/Contents/jbr/Contents/Home"

set -Ux ANDROID_HOME $HOME/Library/Android/sdk
set -Ux PATH $PATH $ANDROID_HOME/emulator
set -Ux PATH $PATH $ANDROID_HOME/platform-tools

set -Ux EDITOR /usr/local/bin/nvim
