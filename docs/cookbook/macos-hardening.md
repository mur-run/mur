# macOS Hardening (Track D §4.6)

Every `.app` produced by `mur agent export --format gui` ships with
Hardened Runtime + a `PrivacyInfo.xcprivacy` manifest, and is run
through Apple's notarization service before users see it. Below is
what we ship, what we deliberately don't, and why.

## What's in the bundle

| Slot                                          | Contents                                                                |
|-----------------------------------------------|-------------------------------------------------------------------------|
| `Contents/MacOS/MurAgent`                      | Tauri 2 main + WebKit webview (Developer ID signed, Hardened Runtime).  |
| `Contents/MacOS/mur-agent-runtime`             | Per-agent supervisor sidecar (signed with the same identity).           |
| `Contents/Resources/PrivacyInfo.xcprivacy`     | Apple privacy manifest declaring the four Required Reason APIs we use.  |
| `Contents/Resources/themes/...`                | Built-in light/dark/high-contrast/solarized/cyberpunk themes.           |
| `Contents/Resources/icon.icns`                 | Bundle icon (per-agent override via `--icon`).                          |

## Why no App Sandbox

App Sandbox blocks `CGEventTap`, which is how the GUI's global
push-to-talk hotkey listens for the user's modifier keypresses. PTT
matters for D1 voice (Kokoro 82M + whisper.cpp), so we adopt the
same model Slack / Linear / Raycast use: **Developer ID + Hardened
Runtime** (no App Sandbox, but every entitlement explicitly declared).

## Hardened Runtime entitlements

Declared in `mur-agent-gui/src-tauri/entitlements.plist`:

| Entitlement                                              | Why                                                                                               |
|----------------------------------------------------------|---------------------------------------------------------------------------------------------------|
| `com.apple.security.cs.allow-jit`                        | WebKit JIT (V8 inside the embedded webview).                                                       |
| `com.apple.security.cs.allow-unsigned-executable-memory` | node / python MCP children may JIT or use unsigned executable memory.                              |
| `com.apple.security.cs.disable-library-validation`       | MCP binaries are user-installed (brew/npm/uv) — not signed by us. Escape hatch only.               |
| `com.apple.security.cs.disable-executable-page-protection` | V8 / Node executable page protection conflict.                                                    |
| `com.apple.security.cs.allow-dyld-environment-variables` | `MUR_*` env vars used by the runtime + agent payload (e.g. `MUR_HOME`).                            |
| `com.apple.security.network.client`                      | A2A peer connections (outbound) — Noise XK over TCP.                                               |
| `com.apple.security.network.server`                      | Inbound TCP+Noise listener for A2A peers.                                                          |
| `com.apple.security.files.user-selected.read-write`      | User-picked file pickers (skill / icon / log export).                                              |

## PrivacyInfo manifest

`mur-agent-gui/src-tauri/Resources/PrivacyInfo.xcprivacy` declares
the four Required Reason APIs we use. **A CI gate
(`scripts/check-required-reason-apis.sh`) fails the build if any of
the desktop-shipped crates uses a Required Reason API category
that isn't declared in the manifest** — this prevents accidental
surface expansion.

| Category                                       | Reason  | Where we use it                                                                          |
|------------------------------------------------|---------|------------------------------------------------------------------------------------------|
| `NSPrivacyAccessedAPICategoryUserDefaults`      | `CA92.1` | `tauri-plugin-store` + WKWebView session prefs.                                          |
| `NSPrivacyAccessedAPICategoryFileTimestamp`     | `C617.1` | Companion-bridge inbox watcher reads mtimes; runtime ledger uses dir-entry mtimes.       |
| `NSPrivacyAccessedAPICategorySystemBootTime`    | `35F9.1` | `Instant::now()` (uptime); tracing subscriber initializes monotonic clock.               |
| `NSPrivacyAccessedAPICategoryDiskSpace`         | `E174.1` | Voice model download free-space precheck; multimodal pipeline temp-dir provisioning.     |

To add a new Required Reason API category:

1. Confirm the API is actually necessary (the CI gate has a list of
   patterns it scans for; if you trip it accidentally, fix the code,
   don't widen the manifest).
2. Find the matching reason code in Apple's
   [Required Reason API list](https://developer.apple.com/documentation/bundleresources/privacy_manifest_files/describing_use_of_required_reason_api).
3. Add the `<dict>` entry to `Resources/PrivacyInfo.xcprivacy` and
   re-run `plutil -lint` on it.
4. Update this cookbook table.
5. Extend the gate's `CHECKS` array in
   `scripts/check-required-reason-apis.sh` with the (regex, category)
   pair so future usage is detected.

### CI gate sharp edges

The gate scans every `.rs/.ts/.tsx` file under the desktop-shipped
crates. Two known sharp edges:

- **Test-only usage trips the gate.** `Instant::now()` shows up in
  tests, examples, and benches. Today that's fine because
  `SystemBootTime` is declared. If a category's only remaining usage
  is in tests and you want to *remove* the manifest entry, you'll
  need to either narrow the SCAN_DIRS or add a `grep -v 'tests/'`
  filter — neither is currently wired in. Easier: keep the
  declaration if the API is genuinely used anywhere shipping or
  not.
- **Manifest deletion silently succeeds.** The gate checks
  manifest-declares-category, not category-still-needed. If you
  remove a `<dict>` block from the manifest AND the corresponding
  code is gone, the gate is silent. Run `plutil -lint` after every
  manifest edit.

## Pipeline phases (export)

When `mur agent export --format gui` runs on macOS with credentials,
the relevant phases are:

```
phase 8  codesign        codesign --options runtime --timestamp --sign $MUR_APPLE_DEVELOPER_ID --entitlements ... --deep
phase 9  notarize        ditto -c -k --keepParent <bundle> <zip> && xcrun notarytool submit <zip> --apple-id $MUR_APPLE_NOTARY_USER --team-id $MUR_APPLE_TEAM_ID --password $MUR_APPLE_NOTARY_KEY --wait
phase 10 staple          xcrun stapler staple <bundle>
phase 11 assess          spctl --assess --type execute --verbose=4 <bundle>
```

Each phase **skips cleanly** when its required env var is unset
(`MUR_APPLE_DEVELOPER_ID` for codesign+assess, `MUR_APPLE_NOTARY_KEY`
for notarize+staple). Dev machines without Apple credentials hit
`--skip-notarize` and the pipeline still produces an unsigned `.app`
suitable for local testing.

## Required env vars (release builds)

| Variable                  | Required for | Notes                                                                                              |
|---------------------------|--------------|----------------------------------------------------------------------------------------------------|
| `MUR_APPLE_DEVELOPER_ID`  | codesign     | Common Name of the Developer ID Application certificate (e.g. `Developer ID Application: Acme...`). |
| `MUR_APPLE_NOTARY_USER`   | notarize     | Apple ID email associated with the Developer Program account.                                      |
| `MUR_APPLE_TEAM_ID`       | notarize     | 10-character Apple Developer team ID.                                                              |
| `MUR_APPLE_NOTARY_KEY`    | notarize     | App-specific password (Apple does NOT accept primary account passwords here).                      |

For the keychain-storage flow (preferred long-term), see
`xcrun notarytool store-credentials --help`. v1 ships the env-var
form because it works in CI without manual keychain provisioning.
