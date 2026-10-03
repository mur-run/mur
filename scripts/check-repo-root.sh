#!/bin/sh
# Reject tracked top-level entries that are not on the repo-root allowlist.
#
# Delegated agents whose sandbox write list held this repo but not the project
# they were working on used the repo root as a scratch area (#1607): build
# output (`target-wt-browser/`, #1602) and other projects' deliverables
# (`ho-me-free.html`, ...) landed here, and some were committed. The delegation
# grant gate (#1610/#1612/#1617) removes the cause; this check is defence in
# depth, so a leak surfaces as a failed commit or CI run instead of a merged PR.
#
# Reads the git INDEX (`git ls-files`), so in a pre-commit hook it sees exactly
# what is about to be committed, staged additions included, and in CI it sees
# the checked-out tree. Untracked and gitignored files are not its concern.
#
# Adding a legitimate new top-level file or directory: add its name to the
# allowlist file below, in the same commit.
#
#     sh scripts/check-repo-root.sh
set -e

repo_root=$(git rev-parse --show-toplevel)
allowlist="${REPO_ROOT_ALLOWLIST:-$repo_root/scripts/repo-root-allowlist.txt}"

if [ ! -f "$allowlist" ]; then
    echo "check-repo-root: allowlist not found: $allowlist"
    exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# One name per line; blank lines and `#` comments ignored.
sed -e 's/#.*//' -e 's/[[:space:]]*$//' "$allowlist" | grep -v '^$' | LC_ALL=C sort -u >"$tmp/allowed"
git -C "$repo_root" ls-files | cut -d/ -f1 | LC_ALL=C sort -u >"$tmp/present"

# Entries present in the index but absent from the allowlist.
strays=$(LC_ALL=C comm -23 "$tmp/present" "$tmp/allowed")

if [ -n "$strays" ]; then
    echo "check-repo-root: new top-level entries not on the allowlist:"
    printf '%s\n' "$strays" | sed 's/^/    /'
    echo "  The repo root is not a scratch area (#1607). If an agent or a build"
    echo "  wrote these, remove them:  git rm -r --cached <name>"
    echo "  If they belong here, add each name to:"
    echo "      ${allowlist#"$repo_root"/}"
    exit 1
fi
