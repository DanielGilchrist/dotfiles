function spotify_player_client_id
  set -l client_id $SPOTIFY_PLAYER_CLIENT_ID

  if test -z "$client_id"
    echo "SPOTIFY_PLAYER_CLIENT_ID isn't set!"
  else
    echo $client_id
  end
end
