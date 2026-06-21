# MUR Hub Self-Updater Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give MUR Hub its own signed, in-app auto-updater (Tauri v2 updater plugin) so it self-updates on launch — instead of the CLI ever touching the `.app`.

**Architecture:** The Hub checks a `latest.json` manifest on GitHub Releases at startup (non-blocking). If a newer signed `MUR Hub.app.tar.gz` exists, it downloads, verifies the minisign signature against an embedded pubkey, swaps the bundle, and relaunches. The existing `release.yml` `hub-macos` job is extended to emit + sign the updater tarball and publish `latest.json`. The Hub version is stamped from the git tag at release time (today it is hardcoded `0.1.0`).

**Tech Stack:** Tauri v2, `tauri-plugin-updater`, `tauri-plugin-process`, minisign (Ed25519), GitHub Releases as the update endpoint, React/Vite frontend (`mur-hub-gui/ui`), GitHub Actions.

## Global Constraints

- **Platform scope: macOS `aarch64` ONLY.** The `hub-macos` job builds only Apple Silicon; there is no Windows/Intel/Linux Hub. The updater is mac-arm64 only.
- **Brand name in user-facing copy is uppercase `MUR`** (productName is already `MUR Hub`). Internal slugs/identifiers stay lowercase (`run.mur.hub`).
- **No hardcoded values** — version comes from the git tag; endpoint/pubkey live in config, not code.
- **Repo slug:** `mur-run/mur`. **Hub bundle identifier:** `run.mur.hub`. **Hub product name:** `MUR Hub`.
- **Tauri signing is mandatory in v2** — the build fails to produce `.sig` artifacts unless `createUpdaterArtifacts: true` AND the private key env vars are present.
- Hub crate is **workspace-excluded**; build/test via `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`. Frontend lives in `mur-hub-gui/ui`.

---

## Task 0: Operator prerequisites (manual, one-time — NOT code)

These cannot be done by an agent and block Tasks 3 and 5. Hand to the repo operator (David).

- [ ] **Generate the updater keypair** (interactive, never commit the private key):

```bash
cd mur-hub-gui/src-tauri
cargo binstall -y tauri-cli --version "^2"   # if not present
cargo tauri signer generate -w ~/.mur-hub-updater.key
# Prompts for a password — remember it.
# Prints the PUBLIC key (a base64 blob) to stdout. Copy it.
```

- [ ] **Add GitHub Actions secrets** (Settings → Secrets and variables → Actions):
  - `TAURI_SIGNING_PRIVATE_KEY` — full contents of `~/.mur-hub-updater.key`
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the password chosen above
- [ ] **Back up `~/.mur-hub-updater.key` + password** in the team password manager. **Losing it means no existing Hub install can ever be updated again.**
- [ ] Hand the **public key** string to whoever implements Task 3 (it goes into `tauri.conf.json`).

---

## Task 1: Stamp the Hub version from the release tag

**Why first:** Both the updater (version compare) and the *already-merged* `mur update` staleness nudge read the Hub version. Today `mur-hub-gui/src-tauri/Cargo.toml` is pinned at `0.1.0` and `lib.rs` writes that to `~/.mur/host_path`, so the nudge **always** fires ("MUR Hub v0.1.0 is out of date"). This task makes the version honest.

**Files:**
- Modify: `.github/workflows/release.yml` (the `hub-macos` job, before `cargo tauri build`)
- Reference (no edit): `mur-hub-gui/src-tauri/Cargo.toml:3`, `mur-hub-gui/src-tauri/tauri.conf.json:4`

**Interfaces:**
- Produces: a Hub `.app` whose `CARGO_PKG_VERSION` and `tauri.conf.json` `version` both equal `${GITHUB_REF_NAME#v}` (e.g. `2.27.0`).

- [ ] **Step 1: Add a version-stamp step** in `release.yml` inside the `hub-macos` job, immediately **before** the `- name: Build, sign, bundle .dmg` step:

```yaml
      - name: Stamp Hub version from tag
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          # tauri.conf.json drives the bundle + updater version
          sed -i '' "s/\"version\": \"[^\"]*\"/\"version\": \"${VERSION}\"/" \
            mur-hub-gui/src-tauri/tauri.conf.json
          # Cargo.toml drives env!("CARGO_PKG_VERSION") written to ~/.mur/host_path
          sed -i '' "0,/^version = \".*\"/s//version = \"${VERSION}\"/" \
            mur-hub-gui/src-tauri/Cargo.toml
          echo "Stamped Hub to ${VERSION}"
          grep -m1 '"version"' mur-hub-gui/src-tauri/tauri.conf.json
          grep -m1 '^version' mur-hub-gui/src-tauri/Cargo.toml
```

> Note: `sed -i ''` is the BSD/macOS form (the `hub-macos` job runs on `macos-14`). Do not use the GNU `sed -i` form here.

- [ ] **Step 2: Verify the regex locally** (cheap, no full build) on a macOS shell:

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cp mur-hub-gui/src-tauri/tauri.conf.json /tmp/t.json
VERSION=2.27.0 bash -c 'sed -i "" "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/" /tmp/t.json'
grep -m1 '"version"' /tmp/t.json   # Expect: "version": "2.27.0"
rm /tmp/t.json
```

Expected: prints `"version": "2.27.0"`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "fix(hub): stamp Hub version from release tag (was pinned 0.1.0)"
```

---

## Task 2: Add updater + process plugins (Rust side)

**Files:**
- Modify: `mur-hub-gui/src-tauri/Cargo.toml` (deps)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:~217` (the `tauri::Builder::default()....plugin(...)` chain)
- Modify: `mur-hub-gui/src-tauri/capabilities/default.json` (permissions)

**Interfaces:**
- Produces: the updater + process plugins registered; the `dashboard` window allowed to check/install updates and restart.

- [ ] **Step 1: Add the crate deps.** In `mur-hub-gui/src-tauri/Cargo.toml`, under `[dependencies]`, add (macOS-gated to match the only build target):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
tauri-plugin-updater = "2"
tauri-plugin-process = "2"
```

- [ ] **Step 2: Register the plugins.** In `mur-hub-gui/src-tauri/src/lib.rs`, in the `tauri::Builder::default()` chain (right after `.plugin(tauri_plugin_shell::init())`), add — macOS-gated so non-mac dev builds still compile:

```rust
        .plugin(tauri_plugin_shell::init())
        // macOS-only: in-app self-update. The updater verifies a minisign
        // signature against the pubkey in tauri.conf.json before swapping the
        // .app; process::restart() relaunches after install.
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
                app.handle().plugin(tauri_plugin_process::init())?;
            }
            #[cfg(not(target_os = "macos"))]
            let _ = app;
            Ok(())
        })
```

> If a `.setup(...)` closure already exists in the chain, fold these two `app.handle().plugin(...)` lines into it instead of adding a second `.setup`.

- [ ] **Step 3: Grant the capability.** In `mur-hub-gui/src-tauri/capabilities/default.json`, add to the `permissions` array:

```json
    "updater:default",
    "process:allow-restart"
```

- [ ] **Step 4: Verify it compiles** (the lib build is what CI runs):

```bash
cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```

Expected: `Finished` with no errors. (First build pulls the new crates — slow once.)

- [ ] **Step 5: fmt + clippy** (CI runs these separately for the excluded crate):

```bash
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml --lib -- -D warnings
```

Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/Cargo.toml mur-hub-gui/src-tauri/Cargo.lock mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/default.json
git commit -m "feat(hub): register updater + process plugins (macOS)"
```

---

## Task 3: Updater config in tauri.conf.json

**Blocked by:** Task 0 (needs the public key).

**Files:**
- Modify: `mur-hub-gui/src-tauri/tauri.conf.json` (`bundle.createUpdaterArtifacts`, new `plugins.updater`)

**Interfaces:**
- Consumes: the public key string from Task 0.
- Produces: a build that emits `MUR Hub.app.tar.gz` + `.sig`; a runtime that checks `https://github.com/mur-run/mur/releases/latest/download/latest.json`.

- [ ] **Step 1: Enable updater artifacts.** In `bundle`, add `"createUpdaterArtifacts": true`:

```json
  "bundle": {
    "active": true,
    "createUpdaterArtifacts": true,
    "targets": ["app", "dmg"],
```

- [ ] **Step 2: Add the updater plugin config.** Add a top-level `"plugins"` object (sibling of `"bundle"`/`"app"`), pasting the real pubkey from Task 0 in place of `PASTE_PUBKEY_FROM_TASK_0`:

```json
  "plugins": {
    "updater": {
      "endpoints": [
        "https://github.com/mur-run/mur/releases/latest/download/latest.json"
      ],
      "pubkey": "PASTE_PUBKEY_FROM_TASK_0"
    }
  }
```

- [ ] **Step 3: Validate JSON:**

```bash
python3 -m json.tool mur-hub-gui/src-tauri/tauri.conf.json > /dev/null && echo OK
```

Expected: `OK`.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/tauri.conf.json
git commit -m "feat(hub): enable updater artifacts + GitHub Releases endpoint"
```

---

## Task 4: Frontend startup check (non-blocking)

**Files:**
- Modify: `mur-hub-gui/ui/package.json` (deps)
- Create: `mur-hub-gui/ui/src/update.ts`
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx` (call on mount)

**Interfaces:**
- Produces: `checkForUpdates(): Promise<void>` in `update.ts`, called once from the dashboard window only (not popover/pet — avoids three concurrent checks).

- [ ] **Step 1: Add the JS plugin deps.**

```bash
cd mur-hub-gui/ui
npm install @tauri-apps/plugin-updater@^2 @tauri-apps/plugin-process@^2
```

- [ ] **Step 2: Create `mur-hub-gui/ui/src/update.ts`:**

```ts
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

// Best-effort startup update check. Any failure (offline, no update, not a
// signed build in dev) is swallowed — the Hub must still open. The Hub owns
// its own update; the `mur` CLI never touches the .app.
export async function checkForUpdates(): Promise<void> {
  try {
    const update = await check();
    if (!update) return;
    console.info(`MUR Hub update available: ${update.version}`);
    await update.downloadAndInstall();
    await relaunch();
  } catch (e) {
    console.warn("update check skipped:", e);
  }
}
```

- [ ] **Step 3: Call it once on dashboard mount.** In `mur-hub-gui/ui/src/components/DashboardApp.tsx`, add the import and a `useEffect` (merge into existing imports / an existing effect if present):

```tsx
import { useEffect } from "react";
import { checkForUpdates } from "../update";

// inside the DashboardApp component body, before return:
  useEffect(() => {
    void checkForUpdates();
  }, []);
```

- [ ] **Step 4: Verify the UI builds:**

```bash
cd mur-hub-gui/ui && npm run build
```

Expected: Vite build succeeds, no type errors.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/package.json mur-hub-gui/ui/package-lock.json mur-hub-gui/ui/src/update.ts mur-hub-gui/ui/src/components/DashboardApp.tsx
git commit -m "feat(hub): check for updates on dashboard launch"
```

---

## Task 5: CI — sign updater artifacts + publish latest.json

**Blocked by:** Task 0 (secrets) and Task 3 (artifacts enabled).

**Files:**
- Modify: `.github/workflows/release.yml` — `hub-macos` job (signing env + latest.json step + uploads) and the `release` job (publish the new files)

**Interfaces:**
- Consumes: `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secrets.
- Produces: release assets `MUR Hub.app.tar.gz`, `MUR Hub.app.tar.gz.sig`, `latest.json`.

- [ ] **Step 1: Pass signing keys to the build.** In the `- name: Build, sign, bundle .dmg` step's `env:` block, add the two signing vars so `cargo tauri build` produces the `.sig`:

```yaml
        env:
          APPLE_TEAM_NAME: ${{ secrets.APPLE_TEAM_NAME }}
          APPLE_TEAM_ID: ${{ secrets.APPLE_TEAM_ID }}
          APPLE_SIGNING_IDENTITY: "Developer ID Application: ${{ secrets.APPLE_TEAM_NAME }} (${{ secrets.APPLE_TEAM_ID }})"
          TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}
          TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD }}
```

- [ ] **Step 2: Collect updater artifacts + build `latest.json`.** Add a new step **after** `- name: Notarize + staple`, **before** `- name: Upload DMG artifact`:

```yaml
      - name: Collect updater artifacts + latest.json
        run: |
          VERSION="${GITHUB_REF_NAME#v}"
          # createUpdaterArtifacts emits the tarball + detached .sig under the bundle dir.
          TARBALL=$(find mur-hub-gui/src-tauri/target -name "*.app.tar.gz" | head -n1)
          SIG=$(find mur-hub-gui/src-tauri/target -name "*.app.tar.gz.sig" | head -n1)
          test -n "$TARBALL" && test -n "$SIG" || { echo "::error::updater artifacts missing — is createUpdaterArtifacts true and the signing key set?"; exit 1; }
          cp "$TARBALL" "dist/MUR Hub.app.tar.gz"
          cp "$SIG"     "dist/MUR Hub.app.tar.gz.sig"
          # latest.json schema: https://v2.tauri.app/plugin/updater/
          cat > dist/latest.json <<EOF
          {
            "version": "${VERSION}",
            "pub_date": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
            "platforms": {
              "darwin-aarch64": {
                "signature": "$(cat "$SIG")",
                "url": "https://github.com/mur-run/mur/releases/download/${GITHUB_REF_NAME}/MUR%20Hub.app.tar.gz"
              }
            }
          }
          EOF
          echo "--- latest.json ---"; cat dist/latest.json
```

- [ ] **Step 3: Upload the new artifacts.** Replace the `- name: Upload DMG artifact` step's `path:` with a multi-line list so the tarball, sig, and manifest ride along:

```yaml
      - name: Upload Hub artifacts
        uses: actions/upload-artifact@v4
        with:
          name: mur-hub-macos
          path: |
            dist/MUR-Hub-aarch64-apple-darwin.dmg
            dist/MUR Hub.app.tar.gz
            dist/MUR Hub.app.tar.gz.sig
            dist/latest.json
```

- [ ] **Step 4: Publish them on the release.** In the `release` job's `- name: Upload to release` `files:` list, add the three new files:

```yaml
          files: |
            *.tar.gz
            *.zip
            *.pkg
            *.dmg
            MUR Hub.app.tar.gz
            MUR Hub.app.tar.gz.sig
            latest.json
            checksums.txt
```

> Note: `*.tar.gz` already globs the Hub updater tarball, but the explicit lines are harmless and document intent. Confirm `softprops/action-gh-release@v2` handles the space in the filename (it does — it takes literal paths). If the space causes trouble in any tool, fall back to a no-space asset name (`MUR-Hub.app.tar.gz`) **and** update the `url` in Step 2 to match.

- [ ] **Step 5: Validate the workflow YAML:**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/release.yml')); print('OK')"
```

Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(hub): sign + publish updater tarball and latest.json"
```

---

## Task 6: End-to-end verification (operator, on next tagged release)

This is the only true integration test — it needs a real signed release. Cannot be unit-tested.

- [ ] **Step 1:** Cut a test tag (e.g. `v2.27.0-rc1`) and let `release.yml` run. Confirm the release has `MUR Hub.app.tar.gz`, its `.sig`, and `latest.json`.
- [ ] **Step 2:** Install the **previous** Hub DMG. Launch it. Confirm `~/.mur/host_path` line 2 shows the previous version (proves Task 1 stamping works).
- [ ] **Step 3:** With `latest.json` pointing at the newer version, relaunch the old Hub. Within a few seconds it should download, swap, and relaunch on the new version. Verify `MUR Hub.app` is still Gatekeeper-valid afterward:

```bash
spctl -a -t exec -vv "/Applications/MUR Hub.app"
```

Expected: `accepted` / `source=Notarized Developer ID`.

- [ ] **Step 4:** Run `mur update` with the now-current Hub installed. Confirm the staleness nudge is **silent** (version match) — proving the merged nudge is now honest.

---

## Self-Review Notes

- **Spec coverage:** version sync (T1) ✓, plugins+caps (T2) ✓, config+pubkey (T3) ✓, frontend check (T4) ✓, CI signing+manifest (T5) ✓, E2E (T6) ✓, keys/secrets (T0) ✓.
- **Known sharp edges flagged inline:** BSD `sed -i ''` on macOS runner; space in `MUR Hub.app.tar.gz` filename (fallback given); macOS-gated deps so non-mac dev builds compile; only `darwin-aarch64` platform key (matches the only build).
- **No new dependency the task couldn't justify:** updater + process are the two official plugins this requires; nothing else added.
- **The already-merged `mur update` nudge** (`mur-core/src/update/mod.rs`) needs **no change** — Task 1 makes its version comparison correct; T6 Step 4 verifies it.
