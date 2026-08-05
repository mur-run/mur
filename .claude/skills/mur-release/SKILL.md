---
name: mur-release
description: Cut a MUR release — bump the workspace version through a PR (both Cargo.lock files); merging to main auto-tags and launches the release. Use when releasing, bumping the version, or tagging vX.Y.Z.
---

# Releasing MUR

`main` is protected: direct pushes are rejected ("Changes must be made through a
pull request", required check "CI pass"). The whole release is therefore one PR:
**merging the bump to `main` IS the release** — `tag.yml` tags the merge commit
and dispatches `release.yml` automatically. There is no manual tag step.

1. **Bump the `Cargo.toml` workspace version** so `mur --version` matches the
   coming tag. CI validates it: a tag whose version disagrees with `Cargo.toml` fails at
   the `validate-version` job. Move both lock files with it — the root one and
   `mur-hub-gui/src-tauri/Cargo.lock`, which keeps its own copy of every workspace
   crate's version and breaks the Hub's release build when it disagrees.
   ```bash
   # `sed -i '' '0,/re/s//repl/'` is a GNU-ism that fails SILENTLY on macOS —
   # exit 0, file unchanged. Edit the line directly and check the result.
   python3 - <<'EOF'
   s = open("Cargo.toml").read()
   assert s.count('version = "X.Y.Z-old"') == 1
   open("Cargo.toml", "w").write(s.replace('version = "X.Y.Z-old"', 'version = "X.Y.Z"', 1))
   EOF
   cargo update --workspace
   cargo update --workspace --manifest-path mur-hub-gui/src-tauri/Cargo.toml
   git checkout -b chore/bump-X.Y.Z
   git add Cargo.toml Cargo.lock mur-hub-gui/src-tauri/Cargo.lock
   git commit -m "chore(release): bump workspace version to X.Y.Z"
   git push -u origin chore/bump-X.Y.Z
   gh pr create --base main --title "chore(release): bump workspace version to X.Y.Z"
   ```
2. **Merge once CI is green — this is the point of no return.** On merge,
   `tag.yml` tags `vX.Y.Z` and dispatches `release.yml`, which publishes to
   crates.io (irreversible; yank-only). Review the bump PR as the release
   approval, because after merge there is no later gate.
3. Verify the automation fired (tag + release run at the tag ref):
   ```bash
   git ls-remote --tags origin vX.Y.Z
   gh run list --workflow=release.yml --limit 1
   ```
   `release.yml` handles the rest: cross-platform build, GitHub Release,
   Homebrew formula update, installer deployment, crates.io publish.
4. Verify the artifact: `brew update && brew upgrade mur`

## Fallback: manual tag (automation down)

A human-pushed tag still fires `release.yml` directly via the push event, and
`tag.yml`'s already-tagged guard keeps the two paths from double-firing. The
old rules apply in full:

- Tag **only a commit that is on `main`** — tagging first strands the tag on a
  commit no branch contains.
- Push the tag **by itself**: `git tag -a vX.Y.Z -m "..." && git push origin vX.Y.Z`.
- Never `git push origin main --tags`: that pushes two refs, and when branch
  protection declines `main` the tag still lands — the error names only the
  rejected branch, so it reads as a clean failure while a release is already
  building from a commit that is not on `main`.
- Local `main` may refuse `git pull --ff-only` when the tree is dirty
  (`pull.rebase=true` routes it through rebase); use
  `git fetch origin && git merge --ff-only origin/main` instead.