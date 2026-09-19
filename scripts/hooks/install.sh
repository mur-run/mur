#!/bin/sh
# Install the repo's git hooks into .git/hooks.
#
# Hooks live in scripts/hooks/ so they are reviewable and shared; git itself
# only ever runs the copies under .git/hooks, which is not versioned. This
# script is the bridge. Re-run it after pulling a hook change.
#
#     sh scripts/hooks/install.sh
#
# Existing hooks are backed up to <name>.bak before being replaced.
set -e

repo_root=$(git rev-parse --show-toplevel)
src="$repo_root/scripts/hooks"
dst="$(git rev-parse --git-path hooks)"

mkdir -p "$dst"

for hook in "$src"/*; do
    name=$(basename "$hook")
    [ "$name" = "install.sh" ] && continue
    [ -f "$hook" ] || continue

    if [ -f "$dst/$name" ] && ! cmp -s "$hook" "$dst/$name"; then
        cp "$dst/$name" "$dst/$name.bak"
        echo "  backed up existing $name -> $name.bak"
    fi

    cp "$hook" "$dst/$name"
    chmod +x "$dst/$name"
    echo "  installed $name"
done

echo "hooks installed into $dst"
