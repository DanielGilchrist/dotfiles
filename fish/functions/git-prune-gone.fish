function git-prune-gone --description "Delete local branches whose upstream is gone (typically merged + auto-deleted on origin). Skips anything currently checked out in a worktree. Runs `git fetch --prune` first to refresh state."
    argparse --name=git-prune-gone 'h/help' 'y/yes' 'n/dry-run' -- $argv
    or return

    if set -q _flag_help
        echo "usage: git-prune-gone [-y] [-n]"
        echo ""
        echo "  Deletes local branches whose upstream is [gone] — i.e. was"
        echo "  pushed to origin at some point and the remote branch no"
        echo "  longer exists. Branches currently checked out in a worktree"
        echo "  are always skipped."
        echo ""
        echo "  -n, --dry-run  — print what would be deleted, don't delete"
        echo "  -y, --yes      — skip the confirmation prompt"
        return 0
    end

    set -l repo (git rev-parse --show-toplevel 2>/dev/null)
    if test -z "$repo"
        echo "git-prune-gone: not in a git repo" >&2
        return 1
    end
    # Resolve to the main worktree so `git branch -D` operates on the
    # authoritative refs.
    set repo (git -C $repo worktree list --porcelain 2>/dev/null | head -1 | string replace -r '^worktree ' '')

    echo "fetching origin (with --prune)…"
    git -C $repo fetch --prune origin 2>/dev/null

    set -l checked_out (git -C $repo worktree list --porcelain 2>/dev/null \
        | string match -r '^branch refs/heads/(.+)$' -g)

    # `git branch -vv`'s first column is a marker (`*`, `+`, ` `), not the
    # name — parsing it is fiddly. `for-each-ref` gives clean output.
    set -l gone (git -C $repo for-each-ref --format='%(refname:short) %(upstream:track)' refs/heads/ \
        | awk '$NF == "[gone]" {print $1}')
    set -l to_delete
    for b in $gone
        contains -- $b $checked_out; and continue
        set -a to_delete $b
    end

    if test (count $to_delete) -eq 0
        echo "nothing to prune"
        return 0
    end

    echo ""
    echo "will delete:"
    for b in $to_delete
        echo "  $b"
    end

    if set -q _flag_dry_run
        return 0
    end

    if not set -q _flag_yes
        echo ""
        read --prompt-str "proceed? [y/N] " --local confirm
        if test "$confirm" != y
            echo "aborted"
            return 1
        end
    end

    for b in $to_delete
        if git -C $repo branch -D $b 2>/dev/null
            echo "  ✓ $b"
        else
            echo "  ✗ $b (git branch -D failed)"
        end
    end
end
