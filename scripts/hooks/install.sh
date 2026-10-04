#!/bin/sh
# Link the repo's git hooks into the hooks directory git actually runs.
#
# Hooks live in scripts/hooks/ so they are reviewable and shared. Each one is
# installed as a symlink back to its tracked file, so a pulled hook change
# takes effect immediately — run this once per clone, not after every update.
#
#     sh scripts/hooks/install.sh
#
# Deliberately not `core.hooksPath scripts/hooks`: that would stop git from
# reading .git/hooks, where MUR's auto-index post-commit hook lives.
#
# An existing hook that is not already our symlink is backed up to <name>.bak.
set -e

# Link into the main worktree, not the current one: hooks are shared by all
# worktrees, and a linked worktree may be deleted later, dangling the link.
repo_root=$(git worktree list --porcelain | sed -n '1s/^worktree //p')
[ -n "$repo_root" ] || repo_root=$(git rev-parse --show-toplevel)
src="$repo_root/scripts/hooks"
# --git-path honours core.hooksPath, so this is where git will look.
dst="$(git rev-parse --path-format=absolute --git-path hooks)"

mkdir -p "$dst"

for hook in "$src"/*; do
    name=$(basename "$hook")
    [ "$name" = "install.sh" ] && continue
    [ -f "$hook" ] || continue

    if [ -L "$dst/$name" ] && [ "$(readlink "$dst/$name")" = "$hook" ]; then
        echo "  $name already linked"
        continue
    fi

    if [ -e "$dst/$name" ] || [ -L "$dst/$name" ]; then
        mv "$dst/$name" "$dst/$name.bak"
        echo "  backed up existing $name -> $name.bak"
    fi

    chmod +x "$hook"
    ln -s "$hook" "$dst/$name"
    echo "  linked $name -> $hook"
done

echo "hooks linked into $dst"
