# Send-from-any-app (Track C3)

A mur agent installed on your machine ought to feel like a teammate the OS already knows — you should be able to throw text, links, screenshots, and PDFs at it from any app you happen to be in, without copy-paste-switch-paste. Track C3 wires four lightweight channels for that. Every channel produces the same `SharePayload` shape and feeds it through the existing B0 multimodal pipeline, so once a payload is in the agent it's tagged `<untrusted_share>` and the next turn's tool use is on a one-turn cooldown.

This cookbook walks the four channels, the per-agent slug constraint that shapes how each channel is registered, the multi-agent escape hatch for hotkey collisions, and what's deliberately deferred to v2.

## URL scheme

Every exported agent registers `muragent-<slug>://share?text=…` as a deep-link scheme. Drop a link in any browser, mail client, or scripting tool that opens URLs, and macOS / Windows / Linux dispatches it to the running agent (or launches it).

```
muragent-coach://share?text=aGVsbG8gd29ybGQ&type=text
```

- `text=` is base64url (`URL_SAFE_NO_PAD`) UTF-8.
- `type=` is `text` (default) or `url`.
- `host` must be `share`. Anything else parses as an error and is dropped silently in production (logged as a warning).

The slug comes from `mur-core::cmd::agent_export_gui::sanitize_for_bundle_id` — it's the kebab-case agent name. So an agent named `Coach Bot 1.0` registers `muragent-coach-bot-1-0://`. The expected slug is baked into the binary at export time; URLs targeting a different slug error out so a malicious page can't blast every running agent at once.

The scheme is registered in `tauri.conf.json`'s `plugins.deep-link.desktop.schemes` (NOT `bundle.macOS.urlSchemes` — Tauri 2 rejects that location). The export pipeline's `phase_4_rewrite_tauri_conf` substitutes `{{AGENT_SLUG}}` with the per-agent slug before the bundle step.

## Global hotkey

Each agent binds `Cmd+Shift+M+<X>` (where `<X>` is the first letter of the slug, uppercased) at startup. Hit the combo and the agent reads your current clipboard, classifies the contents, and ingests:

- text starting with `http(s)://` → `ShareKind::Url`
- other text → `ShareKind::Text`
- image bytes → persisted to a temp PNG → `ShareKind::Image`
- empty clipboard → flashes a "nothing to share" toast (the harness errors; production turns it into a UI nudge)

### Multi-agent collisions

Two agents whose slugs share a first letter (`coach` and `creator`) want the same combo. The escape hatch is per-agent:

```bash
mur agent companion settings <agent> --share-hotkey "CommandOrControl+Alt+K"
```

`resolve_combo(slug, user_override)` reads `share.hotkey` from `~/.mur/agents/<name>/companion/state.yaml` and uses it verbatim when present. Validation of the combo string itself is the GUI's job before persisting; the runtime trusts whatever's in the YAML.

## Services menu

On macOS, the agent registers three entries in the system Services menu via Info.plist's `NSServices` array:

- **Send Selection to {Agent}** — sends highlighted text from any app
- **Send Link to {Agent}** — sends the current selection as a URL
- **Send Image to {Agent}** — sends an image selection (e.g. from Photos, Preview)

Right-click in any app, pick the Services submenu, choose the entry. The selector (`serviceShare:`) reads `NSPasteboard`, decodes the bytes (text via `NSPasteboardTypeString`, image via `NSPasteboardTypePNG`), and ingests as `source = "services"`.

The three entries all dispatch to the same selector — the dispatcher decides what to do based on what's actually in the pasteboard, not which menu item the user picked. This is intentional: Apple's menu items are advisory; the pasteboard is authoritative.

The Info.plist template lives at `mur-agent-gui/src-tauri/Info.plist.template` and `mur-core::cmd::agent_export_gui::rewrite_nsservices` substitutes `{{AGENT_DISPLAY}}` (the human display name, e.g. `Coach`) into the menu titles + `NSPortName` at export time.

## Drag-to-dock

Drag any file onto the agent's dock icon. macOS delivers `application:openFiles:`, Tauri 2 surfaces it as `RunEvent::Opened { urls }`, and we classify each path by extension:

- `png / jpg / jpeg / gif / webp / heic / heif` → `ShareKind::Image` (routed through OCR)
- everything else → `ShareKind::File` (mime-sniffed by `process_artifact`)

A multi-selection lands as multiple separate `SharePayload`s in the ingestor — each one independently wrapped by B0 with the one-turn cooldown.

For the dock icon to highlight on the right kinds, `bundle.fileAssociations` (top level — Tauri 2 puts this *outside* `bundle.macOS`) declares six associations: `text`, `url`, `image`, `png`, `jpeg`, `pdf`. Linux and Windows ignore the macOS specifics here, but the field is harmless.

## React composer treatment

The Rust side emits `share:received` after the ingestor finishes wrapping. The React composer subscribes via `startShareListener(composer)` (from `ui/src/lib/share.ts`) and renders a `ShareBadge` next to the inserted body — amber border-l + soft amber background, mirroring the `<untrusted_share>` trust nudge. Channel labels:

- url_scheme → "Shared via URL scheme"
- hotkey → "Shared via hotkey"
- services → "Shared via Services menu"
- dock → "Shared by dropping on dock"

Click the **Where this came from** accordion to see the raw provenance (e.g. the original `muragent-coach://share?text=...` URL or the bound hotkey combo). Useful when you want to audit a payload that doesn't look quite right.

## Not in v1

Two things are deliberately out of scope and tracked for v2:

- **Unified `mur://` scheme.** Today every agent registers its own `muragent-<slug>://` so the OS can route deep links without a picker. A single `mur://share?...` scheme + an agent picker UI is cleaner but requires a full router (or a system service) and tangles with the install / uninstall lifecycle. Per-agent schemes work today; we'll consolidate later.
- **Share Extension (`.appex`).** macOS's modern Share menu (the iOS-style upward arrow in apps that support it) requires a true `.appex` Share Extension bundled inside the host app. That's a separate Xcode-style bundle target and signing dance; Track C3 sticks with the older Services menu (which works in every Cocoa app today) and defers the `.appex` to v2.

## Acceptance

Run the full harness:

```bash
bash scripts/e2e/c3-send-from-any-app.sh
```

This gates every channel's parser / decoder / classifier and the `SharePayload → SendIngestor → B0` seam. Production wiring (`lib.rs::setup` hooks for hotkey, Services provider, `RunEvent::Opened`; `App.tsx` mount of `startShareListener`) lands in a follow-up PR with its own manual native-channel QA matrix.
