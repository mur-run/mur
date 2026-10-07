#!/bin/sh
# Link the private design-history clone into docs/superpowers/.
#
# Specs, plans and reviews live in the private mur-docs repository, not in this
# public tree (`.gitignore`: `/docs/superpowers/`). Tooling still reads and
# writes them at docs/superpowers/, so each checkout needs that path to be a
# symlink into a mur-docs clone. This script clones mur-docs once (outside the
# repo) and creates the symlink — in this checkout, or in every git worktree.
#
# Git never copies a gitignored path into a new worktree, so run this again
# (or once with --all-worktrees) after `git worktree add`.
#
#     sh scripts/setup-internal-docs.sh                  # this checkout
#     sh scripts/setup-internal-docs.sh --all-worktrees  # every worktree
#     sh scripts/setup-internal-docs.sh --dry-run        # show, change nothing
#
# Environment:
#     MUR_DOCS_DIR     where the mur-docs clone lives   (default: $HOME/mur-docs)
#     MUR_DOCS_REMOTE  what to clone if it is missing   (default: below)
#
# Idempotent: a correct link is left alone. An existing real directory or a
# link pointing elsewhere is never touched — the script reports it and fails.
set -eu

DEFAULT_REMOTE="git@github.com:mur-run/mur-docs.git"
DOCS_SUBDIR="docs/superpowers"
LINK_REL="docs/superpowers"

MUR_DOCS_DIR="${MUR_DOCS_DIR:-$HOME/mur-docs}"
MUR_DOCS_REMOTE="${MUR_DOCS_REMOTE:-$DEFAULT_REMOTE}"

all_worktrees=0
dry_run=0
for arg in "$@"; do
    case "$arg" in
        --all-worktrees) all_worktrees=1 ;;
        --dry-run) dry_run=1 ;;
        -h | --help)
            sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "setup-internal-docs: unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

run() {
    if [ "$dry_run" -eq 1 ]; then
        echo "  (dry-run) $*"
    else
        "$@"
    fi
}

repo_top=$(git rev-parse --show-toplevel 2>/dev/null) || {
    echo "setup-internal-docs: run this from inside the mur repository" >&2
    exit 1
}

# 1. Make sure the mur-docs clone exists.
if [ -d "$MUR_DOCS_DIR/.git" ] || [ -f "$MUR_DOCS_DIR/.git" ]; then
    echo "mur-docs clone: $MUR_DOCS_DIR"
elif [ -e "$MUR_DOCS_DIR" ]; then
    echo "setup-internal-docs: $MUR_DOCS_DIR exists but is not a git clone;" \
        "move it or set MUR_DOCS_DIR" >&2
    exit 1
else
    echo "cloning $MUR_DOCS_REMOTE -> $MUR_DOCS_DIR"
    echo "  (private repo: this needs a GitHub key with access to it)"
    run git clone "$MUR_DOCS_REMOTE" "$MUR_DOCS_DIR"
fi

# 2. Resolve the link target to an absolute path. Worktrees sit at a different
#    depth than the main checkout, so a relative link would break in them.
if [ "$dry_run" -eq 1 ] && [ ! -d "$MUR_DOCS_DIR" ]; then
    target="$MUR_DOCS_DIR/$DOCS_SUBDIR"
else
    if [ ! -d "$MUR_DOCS_DIR/$DOCS_SUBDIR" ]; then
        echo "setup-internal-docs: $MUR_DOCS_DIR has no $DOCS_SUBDIR/" \
            "— is MUR_DOCS_DIR the right repository?" >&2
        exit 1
    fi
    target=$(cd "$MUR_DOCS_DIR/$DOCS_SUBDIR" && pwd -P)
fi

# 3. Link each checkout.
link_one() {
    checkout=$1
    link="$checkout/$LINK_REL"
    if [ -L "$link" ]; then
        current=$(readlink "$link")
        if [ "$current" = "$target" ]; then
            echo "ok       $link"
            return 0
        fi
        echo "CONFLICT $link -> $current (expected $target); left untouched" >&2
        return 1
    fi
    if [ -e "$link" ]; then
        echo "CONFLICT $link is a real directory/file; left untouched" >&2
        return 1
    fi
    run mkdir -p "$(dirname "$link")"
    run ln -s "$target" "$link"
    echo "linked   $link -> $target"
}

status=0
if [ "$all_worktrees" -eq 1 ]; then
    # `git worktree list --porcelain` prints one `worktree <path>` per entry.
    # Paths with newlines are not supported (git itself quotes them).
    worktrees=$(git -C "$repo_top" worktree list --porcelain | sed -n 's/^worktree //p')
    old_ifs=$IFS
    IFS='
'
    for wt in $worktrees; do
        IFS=$old_ifs
        if [ -d "$wt" ]; then
            link_one "$wt" || status=1
        else
            echo "skip     $wt (missing; run 'git worktree prune')"
        fi
    done
    IFS=$old_ifs
else
    link_one "$repo_top" || status=1
fi

exit "$status"
