---
name: mur-release
description: Cut a MUR release — bump the workspace version through a PR (both Cargo.lock files), then tag the commit once it is on main. Use when releasing, bumping the version, or tagging vX.Y.Z.
---

# Releasing MUR

`main` is protected: direct pushes are rejected ("Changes must be made through a
pull request", required check "CI pass"). The version bump therefore goes through
a PR, and **the tag is pushed only after the bump is on `main`** — tagging first
strands the tag on a commit no branch contains.

1. **Bump the `Cargo.toml` workspace version FIRST** so `mur --version` matches the
   tag. CI validates it: a tag whose version disagrees with `Cargo.toml` fails at
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
2. Merge that PR once CI is green, then update your local `main`:
   ```bash
   git checkout main && git pull --ff-only
   ```
3. **Only now** tag the commit that is on `main`, and push the tag by itself:
   ```bash
   git tag -a vX.Y.Z -m "message"
   git push origin vX.Y.Z
   ```
   Never `git push origin main --tags`: that pushes two refs, and when branch
   protection declines `main` the tag still lands — the error names only the
   rejected branch, so it reads as a clean failure while a release is already
   building from a commit that is not on `main`.
4. The release workflow (`release.yml`) handles the rest: cross-platform build,
   GitHub Release, Homebrew formula update, installer deployment, crates.io publish.
5. Verify: `brew update && brew upgrade mur`