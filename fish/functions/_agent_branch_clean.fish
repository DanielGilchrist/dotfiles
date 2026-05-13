function _agent_branch_clean --argument-names repo branch --description "Return 0 if <branch> in <repo> is safe to delete without --force: either zero commits ahead of base, or pushed-and-then-deleted on origin (typical of merge-and-auto-delete)."
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

    # No commits ahead of the base — never had a PR, safe to drop.
    set -l count (git -C $repo rev-list --count "$base..$branch" 2>/dev/null)
    test "$count" = 0; and return 0

    # Has commits ahead. Treat as safe if the branch was pushed to origin (had
    # an upstream) but the remote ref is gone — origin auto-deletes branches
    # on merge in this org, so absent-on-origin implies merged.
    set -l upstream (git -C $repo config --get "branch.$branch.merge" 2>/dev/null)
    test -z "$upstream"; and return 1

    set -l remote_ref (git -C $repo ls-remote --heads origin $branch 2>/dev/null)
    test -z "$remote_ref"
end
