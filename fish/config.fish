set CONFIG_DIR "$HOME/.config"

if test -f $CONFIG_DIR/fish/alias.fish
  source $CONFIG_DIR/fish/alias.fish
end

if test -d $CONFIG_DIR/fish/secret
  for f in $CONFIG_DIR/fish/secret/**/*.fish
    source $f
  end
end

switch (uname)
  case Darwin
    source $CONFIG_DIR/fish/config/macos.fish
  case Linux
    source $CONFIG_DIR/fish/config/linux.fish
end

if test -f "$HOME/.cargo/env.fish"
  source "$HOME/.cargo/env.fish"
end

set -gx MAKEFLAGS "-j$(nproc)"
set -gx XDG_CONFIG_HOME $CONFIG_DIR
set -gx EDITOR (which nvim)
set -gx MANPAGER 'nvim +Man!'
set -gx RAINFROG_CONFIG "$CONFIG_DIR/rainfrog"

set -gx ASDF_CONFIG_FILE "$CONFIG_DIR/.asdfrc"
set -gx ASDF_GOLANG_MOD_VERSION_ENABLED true
set -gx OPENCODE_DISABLE_CLAUDE_CODE true

set -gx PATH $HOME/.asdf/shims $PATH

if status is-interactive
  # Commands to run in interactive sessions can go here
end
