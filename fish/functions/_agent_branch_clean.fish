function _agent_branch_clean --argument-names repo branch --description "Return 0 if <branch> exists in <repo> and has zero commits ahead of its base (origin/HEAD, then origin/main, then origin/master)."
    test -z "$repo" -o -z "$branch"; and return 1
    git -C $repo rev-parse --verify --quiet "refs/heads/$branch" >/dev/null
    or return 1

    set -l base (git -C $repo symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
    if test -z "$base"
        for c in main master
            if git -C $repo rev-parse --verify --quiet "origin/$c" >/dev/null
                set base "origin/$c"
                break
            end
        end
    end
    test -z "$base"; and return 1

    set -l count (git -C $repo rev-list --count "$base..$branch" 2>/dev/null)
    test "$count" = 0
end
