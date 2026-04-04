#!/bin/bash

# Read JSON input from Claude Code
input=$(cat)

# Extract current working directory
cwd=$(echo "$input" | jq -r '.workspace.current_dir')

# Convert home path to ~
display_path=$(echo "$cwd" | sed "s|^$HOME|~|")

# Get git info if in a git repository
git_info=""
if git rev-parse --git-dir > /dev/null 2>&1; then
    branch=$(git branch --show-current 2>/dev/null)
    if [ -n "$branch" ]; then
        # Count modified and untracked files
        modified=$(git status --porcelain 2>/dev/null | grep '^M' | wc -l | tr -d ' ')
        untracked=$(git status --porcelain 2>/dev/null | grep '^?' | wc -l | tr -d ' ')
        git_info=" git $branch $modified $untracked"
    fi
fi

# Get current time in 12-hour format
current_time=$(date '+%l:%M:%S %p' | sed 's/^ *//')

# Output with dimmed colors (will be further dimmed by Claude Code)
printf "%s%s %s" "$display_path" "$git_info" "$current_time"