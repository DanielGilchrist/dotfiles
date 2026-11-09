function heucristic_install --description "Clone heucristic if missing, then build and install it"
  set -l dir "$PERSONAL_DIR/heucristic"
  if not test -d "$dir"
    git clone https://github.com/DanielGilchrist/heucristic.git "$dir"; or return 1
  end

  pushd "$dir"
  asdf install; and script/install
  set -l code $status
  popd
  return $code
end
