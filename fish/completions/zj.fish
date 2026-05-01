function __zj_sessions
    zellij list-sessions -s 2>/dev/null
end

complete -c zj -f
complete -c zj -n '__fish_is_first_token' -a '(__zj_sessions)' -d 'session'
complete -c zj -s h -l help -d 'show help'
