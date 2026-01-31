eval "$(/opt/homebrew/bin/brew shellenv)"
source /opt/homebrew/opt/asdf/libexec/asdf.fish

alias nproc="sysctl -n hw.logicalcpu"

set -gx HOMEBREW_NO_AUTO_UPDATE 1
set -gx JAVA_HOME "/Applications/Android Studio.app/Contents/jbr/Contents/Home"
set -gx ANDROID_HOME $HOME/Library/Android/sdk

set -gx PATH $PATH $ANDROID_HOME/emulator
set -gx PATH $PATH $ANDROID_HOME/platform-tools
