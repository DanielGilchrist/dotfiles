if test -f /usr/local/bin/brew
  eval "$(/usr/local/bin/brew shellenv)"
end

if test -f /usr/local/opt/asdf/libexec/asdf.fish
  source /usr/local/opt/asdf/libexec/asdf.fish
end
