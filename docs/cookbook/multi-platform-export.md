# Cookbook — Multi-platform `mur agent export --format gui`

`mur agent export --format gui` only builds for the **host platform**. The built artifact embeds OS-specific webview bindings (WKWebView on macOS, WebView2 on Windows, WebKitGTK on Linux), and Apple notarization needs a Mac. Cross-compilation is not supported in v1.

To produce all three artifacts (`MyAgent.app`, `MyAgent.AppImage`, `MyAgent.exe`), use a CI matrix that runs the export on each OS in parallel.

## GitHub Actions template

A complete workflow lives at `scripts/templates/agent-export-multi-platform.yml`. Copy it into your repo's `.github/workflows/` and:

1. **Set Actions secrets** (Settings → Secrets and variables → Actions):
   - `APPLE_DEVELOPER_ID` — full Developer ID, e.g. `Developer ID Application: Acme Inc (XXXXXXXXXX)`
   - `APPLE_NOTARY_KEY` — base64 of your `AuthKey_*.p8`
   - `APPLE_NOTARY_KEY_ID` — the 10-char Key ID
   - `APPLE_NOTARY_ISSUER` — App Store Connect issuer UUID
   - `WIN_CERT_THUMBPRINT` — Authenticode cert thumbprint (OV or EV)
2. **Trigger from the Actions tab** (`Build agent app (multi-platform)` workflow), supplying:
   - `agent-pkg-url` — public URL to a `.murpkg` produced earlier with `mur agent export … --format pkg`
   - `theme` (optional, default `light`)
   - `clone-identity` (default false — recommend leaving off for distribution)
3. **Download artifacts** from the workflow run page after each matrix cell completes.

## Runner choices

| OS | Runner image | Why |
|----|--------------|-----|
| macOS | `macos-14` | Apple Silicon — required for `--target universal-apple-darwin` |
| Linux | `ubuntu-22.04` | OLDEST supported; produces AppImage compatible with glibc 2.35+ |
| Windows | `windows-2022` | Modern MSVC + WebView2 SDK |

If you build on `ubuntu-24.04` or newer, the resulting AppImage will not run on older distros — distro stability flows from the build host's glibc version.

## Skipping signing locally

For development:

```bash
mur agent doctor --format gui   # confirm prereqs
mur agent export myagent -o ~/Desktop/MyAgent.app --format gui --skip-notarize
```

The resulting `.app` is unsigned and macOS Gatekeeper will warn on first launch — right-click → "Open" to bypass once. For real distribution, never skip notarization.

## Verifying a signed `.app`

After CI completes:

```bash
spctl --assess --type execute --verbose=4 MyAgent.app
# Should print: source=Notarized Developer ID
```

## Troubleshooting

- **`tauri-cli` not found**: `mur agent doctor --format gui` will flag this. Install with `cargo install tauri-cli --version '^2.0' --locked`.
- **`webkit2gtk` missing**: see the Linux `apt-get install` step in the template.
- **Codesign fails with "errSecInternalComponent"**: keychain not unlocked. CI must `security unlock-keychain` before signing.
- **Notarization stuck "in progress" >30 min**: Apple's notary service has periodic queues. Re-poll with `xcrun notarytool history --key …`.

See `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md` § 8 for the full design rationale.
