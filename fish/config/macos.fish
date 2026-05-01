eval "$(/opt/homebrew/bin/brew shellenv)"

if type -q asdf
    asdf completion fish | source
end

alias nproc="sysctl -n hw.logicalcpu"

set -gx HOMEBREW_NO_AUTO_UPDATE 1
set -gx ANDROID_HOME $HOME/Library/Android/sdk

set -gx PATH $PATH $ANDROID_HOME/emulator
set -gx PATH $PATH $ANDROID_HOME/platform-tools
