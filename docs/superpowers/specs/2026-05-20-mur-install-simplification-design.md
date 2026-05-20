# MUR Install Simplification — Design Spec

**Date**: 2026-05-20
**Status**: Draft

## Goal

Reduce install friction for MUR CLI. Current options (Homebrew macOS-ARM-only, cargo
from git, or manual build) exclude too many potential users. Add a one-liner curl
installer for all platforms, a signed DMG for macOS, crates.io publishing, and a
built-in self-update mechanism.

## Non-Goals

- x86_64 macOS builds (Apple Silicon only for DMG/pkg)
- apt/yum repositories, winget manifests, Scoop buckets, npm wrappers
- GUI installer wizards (the `.pkg` macOS installer is as far as we go)

---

## 1. Architecture Overview

Four new deliverables plus two existing ones, all sourced from GitHub Releases:

```
                    GitHub Releases (existing)
                    ├── mur-aarch64-apple-darwin.tar.gz
                    ├── mur-x86_64-unknown-linux-gnu.tar.gz
                    ├── mur-x86_64-pc-windows-msvc.zip
                    ├── checksums.txt
         New →      ├── mur-aarch64-apple-darwin.pkg          ← signed macOS pkg
         New →      └── mur-aarch64-apple-darwin.dmg          ← DMG wrapping the pkg

    mur.run (GitHub Pages)                  crates.io (New)
    ├── /install.sh    (New → curl | sh)    └── mur crate
    └── /install.ps1   (New → irm | iex)

    Homebrew Tap (existing)
    └── mur-run/tap/mur.rb

    Built-in CLI (New)
    └── mur update  →  GitHub Releases API → download + replace binary
```

All install methods fetch binaries from GitHub Releases — the single source of truth.
`install.sh` / `install.ps1` provide the widest coverage. DMG provides a polished
macOS-native experience. `mur update` handles ongoing updates for non-package-manager
installs.

---

## 2. curl Installer (install.sh)

Hosted at `https://mur.run/install.sh`. POSIX sh, no bash-isms.

### Flow

```
curl -fsSL https://mur.run/install.sh | sh

  1. Detect OS + arch (uname -s / uname -m)
  2. Map to release asset:
       Darwin arm64  → mur-aarch64-apple-darwin.tar.gz
       Linux x86_64  → mur-x86_64-unknown-linux-gnu.tar.gz
       Linux aarch64 → not yet built (print cargo install fallback + exit)
       *MINGW*/MSYS   → redirect to install.ps1
  3. Query GitHub API: GET /repos/mur-run/mur/releases/latest
  4. Download asset + checksums.txt
  5. sha256sum verification
  6. Extract to temp dir
  7. Install binary to ${MUR_INSTALL_DIR:-$HOME/.local/bin}
     (fallback /usr/local/bin if ~/.local/bin unwritable)
  8. Warn if INSTALL_DIR is not on PATH; print export line
  9. Print "Run 'mur init' to get started"
```

### Flags

- `--version X.Y.Z` — install specific version instead of latest
- `-s` — silent (no stdout except errors; for CI)
- `MUR_INSTALL_DIR` env var overrides install path

### Windows (install.ps1)

```powershell
irm https://mur.run/install.ps1 | iex
```

Same logic: detect arch → download zip from GitHub Releases → extract to
`$HOME/.local/bin` → add to User PATH if needed.

---

## 3. DMG + .pkg (macOS)

A signed `.pkg` installer wrapped in a signed + notarized `.dmg`. Both produced
on the macOS CI runner during the release workflow.

### .pkg Structure

```
mur-aarch64-apple-darwin.pkg
├── mur binary → /usr/local/bin/mur
├── LICENSE     → /usr/local/share/doc/mur/LICENSE
├── preinstall  → ensure /usr/local/bin exists and is writable
└── postinstall → print "Run 'mur init' to get started"
```

Built with `pkgbuild`:

```bash
pkgbuild --root pkg-root \
  --identifier run.mur.cli \
  --version "$VERSION" \
  --scripts scripts/ \
  --install-location / \
  mur.pkg
```

### DMG Creation

Simple DMG — just a container for the `.pkg`. No custom background image or
drag-to-Applications arrow (those conventions are for GUI .app bundles).

```bash
hdiutil create -volname "MUR" -srcfolder dmg-root -format UDZO mur.dmg
```

### Signing + Notarization (CI)

```bash
# 1. Sign the .pkg
productsign --sign "Developer ID Installer: ..." mur.pkg mur-signed.pkg

# 2. Wrap in DMG
hdiutil create ... mur.dmg

# 3. Sign the DMG
codesign --sign "Developer ID Application: ..." mur.dmg

# 4. Notarize
xcrun notarytool submit mur.dmg --apple-id ... --team-id ... --password ... --wait

# 5. Staple (offline notarization ticket)
xcrun stapler staple mur.dmg
```

### Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| DMG background image | None | CLI tool; DMG is a delivery vehicle, not a marketing surface |
| .pkg contents | binary + LICENSE only | Minimal; no extra files |
| Install path | `/usr/local/bin` | Standard macOS CLI path (same as Homebrew) |
| Certificates | Developer ID Installer (.pkg) + Developer ID Application (.dmg) | Apple requirement |
| Notarization | Mandatory | Otherwise Gatekeeper blocks execution |
| CI secrets | APPLE_SIGNING_CERT, APPLE_KEYCHAIN_PASSWORD, APPLE_NOTARY_USER, APPLE_TEAM_ID, APPLE_NOTARY_PASSWORD | Stored in GitHub Secrets |

---

## 4. Self-Update (`mur update`)

New `mur-core/src/cmd/update.rs` module (~150 lines). Registered as a top-level
subcommand.

### Install Source Detection

```
mur update
  → detect install source:
      brew list mur succeeds       → print "Use 'brew upgrade mur'" + exit
      cargo install --list has mur → print "Use 'cargo install mur'" + exit
      otherwise (curl/pkg/manual)  → perform self-update
```

### Self-Update Flow

```
  1. GET https://api.github.com/repos/mur-run/mur/releases/latest
  2. Compare tag_name with current version
     Same → "Already up to date (vX.Y.Z)" + exit 0
  3. Select matching asset from the release (by OS + arch)
  4. Download asset + checksums.txt to temp dir
  5. SHA256 verification
  6. Extract binary to temp file
  7. Replace current binary:
     Unix:     mv new-binary old-binary  (atomic on same filesystem)
     Windows:  spawn PowerShell helper, exit; helper sleeps 2s then replaces
  8. Print new version
```

### Windows Helper Script

Windows locks running executables. `mur update` generates a temporary script
and spawns it detached before exiting:

```powershell
# Generated by mur update, executed after mur.exe exits
Start-Sleep -Seconds 2
Move-Item -Force $newExe $targetExe
Remove-Item $scriptPath
```

### CLI Surface

```
mur update              Check for updates + install if newer
mur update --check      Dry-run: report available version, don't install
```

### Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Release source | GitHub Releases API | Single source of truth |
| Package manager detection | Query system (brew list, cargo install --list) | Simple; no sentinel files needed |
| Unix binary swap | `mv` (same filesystem) | Atomic, no helper required |
| Windows binary swap | PowerShell helper spawn | Only reliable approach for locked .exe |
| Old version backup | None | Any version downloadable from GitHub Releases |
| API rate limit | Cache latest-release response for 5 min | Prevents spurious rate limiting |
| Fallback binary path | Read `/proc/self/exe` (Linux/macOS) / `GetModuleFileNameW` (Windows) | Reliable, cross-platform |

---

## 5. CI/CD Changes

### release.yml Updates

Insert `package-macos` job between `build` and `release`:

```yaml
  package-macos:
    name: Package macOS (DMG + PKG)
    needs: build
    runs-on: macos-latest
    if: always() && needs.build.result == 'success'
    steps:
      - uses: actions/download-artifact@v4
        with: { name: mur-aarch64-apple-darwin }
      - name: Extract binary
        run: tar xzf mur-aarch64-apple-darwin.tar.gz
      - name: Import Apple signing certs
        uses: apple-actions/import-codesign-certs@v2
        with:
          p12-file-base64: ${{ secrets.APPLE_SIGNING_CERT }}
          p12-password: ${{ secrets.APPLE_KEYCHAIN_PASSWORD }}
      - name: Build .pkg
        run: |
          mkdir -p pkg-root/usr/local/bin
          mkdir -p pkg-root/usr/local/share/doc/mur
          cp mur pkg-root/usr/local/bin/mur
          cp LICENSE pkg-root/usr/local/share/doc/mur/LICENSE
          mkdir -p scripts
          cat > scripts/postinstall << 'EOF'
          #!/bin/sh
          echo "MUR v$INSTALLED_VERSION installed to /usr/local/bin/mur"
          echo "Run 'mur init' to get started."
          EOF
          chmod +x scripts/postinstall
          pkgbuild --root pkg-root \
            --identifier run.mur.cli \
            --version "$VERSION" \
            --scripts scripts/ \
            --install-location / \
            mur.pkg
      - name: Sign .pkg
        run: |
          productsign --sign "Developer ID Installer" mur.pkg mur-signed.pkg
      - name: Create DMG
        run: |
          mkdir dmg-root
          cp mur-signed.pkg dmg-root/
          hdiutil create -volname "MUR" -srcfolder dmg-root -format UDZO mur.dmg
      - name: Sign + Notarize DMG
        run: |
          codesign --sign "Developer ID Application" mur.dmg
          xcrun notarytool submit mur.dmg \
            --apple-id "$APPLE_ID" \
            --team-id "$TEAM_ID" \
            --password "$NOTARY_PWD" \
            --wait
          xcrun stapler staple mur.dmg
        env:
          APPLE_ID: ${{ secrets.APPLE_NOTARY_USER }}
          TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          NOTARY_PWD: ${{ secrets.APPLE_NOTARY_PASSWORD }}
      - name: Upload DMG + PKG
        uses: actions/upload-artifact@v4
        with:
          name: mur-macos-installer
          path: |
            mur-signed.pkg
            mur.dmg
```

Updated `release` job picks up the new DMG/PKG artifacts.

### install.sh Deployment

New `deploy-installer` job at end of release workflow:

```yaml
  deploy-installer:
    name: Deploy install.sh to mur.run
    needs: release
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - name: Push install.sh to gh-pages
        run: |
          cp scripts/install.sh /tmp/install.sh
          cp scripts/install.ps1 /tmp/install.ps1
          git fetch origin gh-pages
          git checkout gh-pages
          cp /tmp/install.sh install.sh
          cp /tmp/install.ps1 install.ps1
          git add install.sh install.ps1
          git diff --cached --quiet && exit 0
          git commit -m "install.sh for v$VERSION"
          git push origin gh-pages
```

`mur.run` DNS: CNAME to `<org>.github.io/mur`. Installer scripts live at the
repo root on the gh-pages branch, served at `mur.run/install.sh` and
`mur.run/install.ps1`. **Prerequisite:** DNS CNAME record must be configured
before the deploy-installer job can be tested end-to-end. GitHub Pages also
needs the custom domain configured in the repo settings.

### crates.io Publishing

```yaml
  publish-crates:
    name: Publish to crates.io
    needs: release
    runs-on: ubuntu-latest
    if: always() && needs.release.result == 'success'
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo publish -p mur-common --token ${{ secrets.CRATES_IO_TOKEN }}
      - run: cargo publish -p mur-core --token ${{ secrets.CRATES_IO_TOKEN }}
```

### Full Release Pipeline (after changes)

```
Tag push v*
  → Build (matrix: aarch64-macOS, x64-linux, x64-windows)
  → Package macOS (DMG + PKG + sign + notarize)         ← New
  → Publish GitHub Release (including DMG + PKG assets) ← Updated
  → Update Homebrew Tap                                  (unchanged)
  → Deploy install.sh to mur.run (gh-pages)              ← New
  → Publish to crates.io                                 ← New
```

### Required GitHub Secrets

| Secret | Purpose |
|---|---|
| `APPLE_SIGNING_CERT` | base64-encoded p12 certificate |
| `APPLE_KEYCHAIN_PASSWORD` | p12 decryption password |
| `APPLE_NOTARY_USER` | Apple ID email for notarization |
| `APPLE_TEAM_ID` | Apple Developer Team ID |
| `APPLE_NOTARY_PASSWORD` | App-specific password for Apple ID |
| `CRATES_IO_TOKEN` | crates.io API token |
| `HOMEBREW_TAP_TOKEN` | (existing) Homebrew tap push token |

---

## 6. crates.io Metadata

Add missing metadata to `Cargo.toml`:

```toml
[package]
description = "Invisible continuous learning system for AI coding assistants"
keywords = ["ai", "coding", "cli", "learning", "patterns"]
categories = ["command-line-utilities", "development-tools"]
license = "MIT"
```

Publishing order: `mur-common` first, then `mur-core`.
`mur-core`'s path dependency on `mur-common` (`mur-common = { path = "../mur-common" }`)
must change to a versioned dependency (`mur-common = "2.16"`) before publishing.

---

## 7. Error Handling

### install.sh

- GitHub API unreachable → "Could not reach GitHub. Check your connection."
- Unknown OS/arch → "No prebuilt binary for <os>/<arch>. Install from source: cargo install mur"
- Checksum mismatch → "Checksum verification FAILED. Aborting." (exit 1)
- Download interrupted → clean up temp dir, exit 1
- INSTALL_DIR not writable → print sudo fallback suggestion

### mur update

- GitHub API unreachable → "Could not check for updates. Are you online?"
- GitHub API rate limited → "GitHub API rate limit reached. Try again in N minutes."
- Binary path detection failure → "Cannot determine install location. Please reinstall via: curl -fsSL https://mur.run/install.sh | sh"
- Windows: PowerShell not found → "On Windows, mur update requires PowerShell to complete the update."

---

## 8. Testing Strategy

### install.sh

- Test on clean Docker images: ubuntu:latest, alpine:latest, archlinux:latest
- Test on macOS runner in CI
- Test checksum verification with intentional mismatch
- Test `--version` flag with known release
- Test PATH detection (missing, present, duplicate)

### mur update

- Unit test: version comparison logic
- Unit test: asset selection by OS/arch
- Integration test (CI only): download from test release, verify checksum, verify binary swap
- Manual test on Windows: verify PowerShell helper spawns correctly

### DMG + PKG

- CI: verify DMG mounts without error
- CI: verify .pkg installs binary to correct path
- CI: verify `spctl -a -t open --context context:primary-signature mur.dmg` passes

---

## 9. Rollout Sequence

1. **Add `mur update` command** — isolated feature, can merge before everything else
2. **Add `install.sh` + `install.ps1`** — commit scripts, set up gh-pages deploy
3. **Update release.yml** — add DMG/PKG packaging, signing, notarization
4. **Update Cargo.toml + publish to crates.io** — metadata + first publish
5. **Update README.md** — replace "Other install methods" section with new options
6. **Acquire Apple Developer membership** — before step 3 can complete in CI
