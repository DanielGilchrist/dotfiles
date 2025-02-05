if test -f /opt/homebrew/bin/brew
  eval "$(/opt/homebrew/bin/brew shellenv)"
end

if test -f /opt/homebrew/opt/asdf/libexec/asdf.fish
  source /opt/homebrew/opt/asdf/libexec/asdf.fish
end
