# Unified Channel v4d — APNs Offline Push — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements `2026-06-16-unified-channel-v4-ios-design.md` §6 v4d. **Depends on v4a** (mobile sync + the channel watcher → connected-phone push) and **v4b/v4c** (the phone UI + HITL respond). Delivers the deferred "be woken while the app is closed" behavior.

**Goal:** wake the phone via **APNs silent push** when a channel the owner cares about changes (especially a HITL gate going `input-required`) while the MUR app is **not connected** — then the phone reconnects over its signed WebSocket and fetches the actual data. Today the phone only receives over a live WebSocket; close the app and you miss everything until you reopen it. This closes the most valuable mobile gap.

**Architecture:** the daemon sends APNs **directly** (outbound HTTPS to `api.push.apple.com`), so push works even when the phone is off-LAN and the daemon is at home — **no dependency on the external Go relay**. The push is a **silent/content-available** notification carrying only a channel id (no plaintext content) — the phone, on receipt, reconnects its signed tunnel and pulls events via v4a's `channels/events` (since last-seen `seq`). The daemon learns each phone's APNs device token over the existing paired connection (a new `RegisterPush` frame), stores it in `paired.json` (schema migrated from a bare pubkey array to records), tracks which paired devices are currently **connected**, and on a channel change pushes to the **disconnected** ones. APNs auth uses a token-based **.p8** key (key id + team id + topic = bundle id) from `config.yaml`.

**Tech Stack:** Rust (`mur-core` paired-store + push payload; `mur-daemon` APNs sender + watcher trigger + `RegisterPush` handler; `mur-mobile-sdk` register method) + an APNs HTTP/2 client (the `a2` crate, new dep) + SwiftUI/UIKit (`mur-mobile-app`: push capability, `AppDelegate`, registration, silent-push handler). Builds on v4a–v4c.

**Scope guardrails (v4 spec §6 v4d, §7):**
- **Silent push only, no plaintext** — the payload carries a channel id + `content-available: 1`; real data is fetched over the signed tunnel after reconnect. (A user-visible alert for a HITL gate is an optional follow-on once silent wake works.)
- **Daemon-direct APNs** (outbound HTTPS); the external Go relay is NOT modified.
- Push is a **wake-up fallback**, not a transport: the live WebSocket (v4a) stays primary; push only fires for **disconnected** paired devices.
- **External, out-of-repo prerequisites** (call out, don't implement): an Apple Developer **APNs Auth Key (.p8)** + key id + team id; enabling the **Push Notifications** capability on app id `run.mur.voice`. The plan wires everything that consumes these; provisioning is manual.

**Key facts locked during exploration (do not re-derive):**
- **No push infra exists** anywhere (no APNs/`UNUserNotificationCenter`/`registerForRemoteNotifications`/`device_token`/entitlements/`UIBackgroundModes`). The iOS app has **no `AppDelegate`** (pure SwiftUI `@main` in `MurVoiceApp.swift`); `Info.plist` has only camera/mic/speech usage strings; `project.yml` bundle `run.mur.voice`, team `JQ2C2UA8JV`.
- Paired devices: `~/.mur/mobile/paired.json` = a JSON **array of multibase Ed25519 pubkey strings** (`mur-core/src/mobile.rs`, `load_paired`/`save_paired`; daemon loads at `mobile_server.rs:94`). No token/online state.
- The channel watcher: `mur_channel::watch::watch_channels(mur_home, on_change: Fn(String))` (`watch.rs:9`) — the daemon already spawns it and broadcasts `channel.updated` to **connected** sockets (`mobile_server.rs` select loop). v4d adds the **disconnected** branch.
- `ClientFrame`/`ServerFrame` in `mur-common/src/mobile.rs` (`:26`/`:52`); frames are authenticated by the paired connection. The daemon knows a device is **connected** while its `handle_socket` task is alive (the paired pubkey is in the in-memory connected set).
- Relay server = external Go on Fly.io (`relay.mur.run`), out of this repo — v4d does not touch it.

---

## File Structure

**Created:**
- `mur-core/src/mobile_push.rs` — `PairedDevice` record + migrated paired-store load/save; `silent_push_payload(channel_id)`.
- `mur-daemon/src/apns.rs` — the APNs HTTP/2 sender (`a2`-backed) + `ApnsConfig`.

**Modified:**
- `mur-common/src/mobile.rs` — `ClientFrame::RegisterPush {device_token, environment}`.
- `mur-common/src/config.rs` — `ApnsConfig {auth_key_p8_path, key_id, team_id, topic, environment}` under mobile config.
- `mur-mobile-sdk/src/lib.rs`,`transport.rs` — `register_push(device_token, environment)`.
- `mur-daemon/src/mobile_server.rs` — handle `RegisterPush`; track connected pubkeys; on watcher change, APNs-push disconnected devices.
- `mur-daemon/src/main.rs` — construct the APNs sender from config; share into `MobileState`.
- `mur-mobile-app/project.yml` + `Info.plist` + new `AppDelegate.swift`/`MurVoiceApp.swift` — push capability, background mode, registration, silent-push handler.

---

## Task 1: Paired-store schema migration (pubkey → record)

**Files:**
- Create: `mur-core/src/mobile_push.rs`; Modify: `mur-core/src/mobile.rs` (paired load/save)

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/mobile_push.rs`:

```rust
//! Paired-device records + APNs push payloads (v4d). Migrates the legacy
//! `paired.json` (a bare array of pubkey strings) to records that also carry an
//! APNs device token, kept backward-compatible on read.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PairedDevice {
    /// Multibase Ed25519 pubkey — the stable device identity.
    pub pubkey: String,
    /// APNs device token (hex), set via RegisterPush. None = no push yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apns_token: Option<String>,
    /// "sandbox" | "production".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apns_env: Option<String>,
}

/// Parse `paired.json` content tolerating BOTH the legacy `["zPub…", …]` array
/// and the new `[{"pubkey":…}, …]` records.
pub fn parse_paired(content: &str) -> Vec<PairedDevice> {
    if let Ok(recs) = serde_json::from_str::<Vec<PairedDevice>>(content) {
        return recs;
    }
    if let Ok(keys) = serde_json::from_str::<Vec<String>>(content) {
        return keys.into_iter().map(|pubkey| PairedDevice { pubkey, apns_token: None, apns_env: None }).collect();
    }
    Vec::new()
}

/// A silent (content-available) APNs payload carrying only a channel id — no
/// plaintext. The phone reconnects + fetches the real data over the signed tunnel.
pub fn silent_push_payload(channel_id: &str) -> serde_json::Value {
    serde_json::json!({
        "aps": { "content-available": 1 },
        "mur": { "channel_id": channel_id }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_paired_reads_legacy_and_new() {
        let legacy = r#"["zAlice","zBob"]"#;
        let v = parse_paired(legacy);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].pubkey, "zAlice");
        assert_eq!(v[0].apns_token, None);

        let new = r#"[{"pubkey":"zAlice","apns_token":"deadbeef","apns_env":"sandbox"}]"#;
        let v2 = parse_paired(new);
        assert_eq!(v2[0].apns_token.as_deref(), Some("deadbeef"));
        assert_eq!(v2[0].apns_env.as_deref(), Some("sandbox"));

        assert!(parse_paired("garbage").is_empty());
    }

    #[test]
    fn silent_payload_is_content_available_and_carries_channel() {
        let p = silent_push_payload("c1");
        assert_eq!(p["aps"]["content-available"], 1);
        assert_eq!(p["mur"]["channel_id"], "c1");
        assert!(p.get("alert").is_none(), "silent: no user-visible alert");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core mobile_push::` — Expected: FAIL (module not declared).

- [ ] **Step 3: Wire the module + migrate the paired store**

Declare `pub mod mobile_push;` in `mur-core/src/lib.rs`. Update `mur-core/src/mobile.rs`'s paired load/save to use `PairedDevice` (read via `parse_paired`; write the record form). Add helpers `set_apns_token(home, pubkey, token, env)` and `paired_devices(home) -> Vec<PairedDevice>` so the daemon can look up tokens. Keep `is_paired(home, pubkey)` working (now checks records).

- [ ] **Step 4: Run + commit**

Run: `cargo test -p mur-core mobile_push:: && cargo test -p mur-core mobile::` — Expected: PASS (legacy `paired.json` still loads).

```bash
git add mur-core/src/mobile_push.rs mur-core/src/mobile.rs mur-core/src/lib.rs
git commit -m "feat(mobile): paired-device records + silent push payload (v4d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: `RegisterPush` frame + handler + SDK method

**Files:**
- Modify: `mur-common/src/mobile.rs`; `mur-daemon/src/mobile_server.rs`,`relay_client.rs`; `mur-mobile-sdk/src/lib.rs`,`transport.rs`

- [ ] **Step 1: Add the frame**

In `mur-common/src/mobile.rs`, add to `ClientFrame`:

```rust
    /// Register this device's APNs token so the daemon can wake it while offline.
    RegisterPush { device_token: String, environment: String },
```

- [ ] **Step 2: Handle it (both daemon paths)**

In `mobile_server.rs::handle_socket`, the connection knows the paired pubkey (from `Hello`). Add an arm:

```rust
            ClientFrame::RegisterPush { device_token, environment } => {
                if let Some(pubkey) = &connection_pubkey {
                    mur_core::mobile::set_apns_token(state.mur_home.as_path(), pubkey, &device_token, &environment);
                }
            }
```

(`connection_pubkey` = the pubkey captured at `Hello`. If the handler doesn't already retain it, store it in a local after the `Hello` arm.) Mirror in `relay_client.rs`.

- [ ] **Step 3: SDK method**

In `mur-mobile-sdk/src/lib.rs`: `pub fn register_push(&self, device_token: String, environment: String) -> Result<(), SdkError>` → `Command::RegisterPush`; `transport.rs` serializes to `ClientFrame::RegisterPush`.

- [ ] **Step 4: Build + commit**

Run: `cargo build -p mur-core -p mur-daemon -p mur-mobile-sdk` — Expected: compiles.

```bash
git add mur-common/src/mobile.rs mur-daemon/src/mobile_server.rs mur-daemon/src/relay_client.rs mur-mobile-sdk/src/lib.rs mur-mobile-sdk/src/transport.rs
git commit -m "feat(mobile): RegisterPush frame stores the device's APNs token (v4d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: APNs sender + offline push trigger (daemon)

**Files:**
- Create: `mur-daemon/src/apns.rs`; Modify: `mur-common/src/config.rs`, `mur-daemon/src/mobile_server.rs`, `mur-daemon/src/main.rs`, `mur-daemon/Cargo.toml`

- [ ] **Step 1: Config + APNs sender**

Add to `mur-common/src/config.rs` (under the mobile config block):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ApnsConfig {
    /// Path to the APNs Auth Key (.p8). Empty = push disabled.
    pub auth_key_p8_path: String,
    pub key_id: String,
    pub team_id: String,
    /// APNs topic = the app bundle id, e.g. "run.mur.voice".
    pub topic: String,
    /// "sandbox" | "production".
    pub environment: String,
}
```

Create `mur-daemon/src/apns.rs` using the `a2` crate (add `a2 = "0.10"` to `mur-daemon/Cargo.toml` — a maintained token-auth APNs client):

```rust
//! Token-authenticated (.p8) APNs sender. Sends silent (content-available)
//! pushes so a disconnected phone reconnects + fetches over the signed tunnel.

use anyhow::{Context, Result};
use mur_common::config::ApnsConfig;

pub struct Apns {
    client: a2::Client,
    topic: String,
}

impl Apns {
    /// Build from config; returns None when push is not configured (no .p8 path).
    pub fn from_config(cfg: &ApnsConfig) -> Option<Self> {
        if cfg.auth_key_p8_path.is_empty() { return None; }
        let endpoint = if cfg.environment == "production" { a2::Endpoint::Production } else { a2::Endpoint::Sandbox };
        let mut key = std::fs::File::open(&cfg.auth_key_p8_path).ok()?;
        let client = a2::Client::token(&mut key, &cfg.key_id, &cfg.team_id, a2::ClientConfig::new(endpoint)).ok()?;
        Some(Self { client, topic: cfg.topic.clone() })
    }

    /// Send a silent push to `device_token` waking it for `channel_id`.
    pub async fn wake(&self, device_token: &str, channel_id: &str) -> Result<()> {
        let payload = mur_core::mobile_push::silent_push_payload(channel_id);
        let opts = a2::request::notification::NotificationOptions {
            apns_topic: Some(&self.topic),
            apns_push_type: Some(a2::request::notification::PushType::Background),
            ..Default::default()
        };
        let builder = a2::request::payload::Payload::from_json(device_token, &payload, opts)
            .context("build apns payload")?;
        self.client.send(builder).await.context("apns send")?;
        Ok(())
    }
}
```

(If the `a2` API differs by version, adapt — the contract is: token auth from the .p8, background/content-available push, per-device-token send. Pin the version you add.)

- [ ] **Step 2: Track connected devices + push the disconnected on change**

In `mobile_server.rs`, maintain a shared `connected: Arc<Mutex<HashSet<String>>>` (paired pubkeys with a live socket) in `MobileState`: insert on `Hello`, remove when `handle_socket` returns. In the channel-watcher branch (the daemon already gets `on_change(channel_id)` → broadcasts to connected sockets), add: for each `PairedDevice` with an `apns_token` whose pubkey is **NOT** in `connected`, call `apns.wake(token, &channel_id)`:

```rust
// In the watcher callback / a task that owns `state` + the Apns sender:
let online = state.connected.lock().unwrap().clone();
for dev in mur_core::mobile::paired_devices(state.mur_home.as_path()) {
    if let (Some(tok), false) = (dev.apns_token.as_deref(), online.contains(&dev.pubkey)) {
        if let Some(apns) = &state.apns {
            let apns = apns.clone(); let tok = tok.to_string(); let cid = channel_id.clone();
            tokio::spawn(async move { if let Err(e) = apns.wake(&tok, &cid).await { tracing::warn!("apns wake failed: {e:#}"); } });
        }
    }
}
```

(Make `Apns` shareable — wrap in `Arc`. The watcher already runs; this extends its callback. Throttle/coalesce per device if a channel is chatty — a simple per-(device,channel) debounce is a reasonable add.)

- [ ] **Step 3: Construct the sender in `main.rs`**

Where the daemon builds `MobileState` (near the mobile-server/relay spawn), construct `Arc<Apns>` from `config.mobile.apns` (if `Apns::from_config` returns `Some`) and put it + the `connected` set into `MobileState`.

- [ ] **Step 4: Build + commit**

Run: `cargo build -p mur-daemon` — Expected: compiles (with `a2` added). Unit-test the payload (Task 1 covers `silent_push_payload`); the real send is integration (Task 5).

```bash
git add mur-daemon/src/apns.rs mur-daemon/Cargo.toml mur-common/src/config.rs mur-daemon/src/mobile_server.rs mur-daemon/src/main.rs
git commit -m "feat(daemon): APNs sender + wake disconnected devices on channel change (v4d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: iOS push registration + silent-push handler

**Files:**
- Modify: `mur-mobile-app/project.yml`, `mur-mobile-app/Sources/Info.plist`, `MurVoiceApp.swift`; Create: `mur-mobile-app/Sources/AppDelegate.swift`

> External prerequisite (manual, Apple Developer): enable the **Push Notifications** capability for app id `run.mur.voice` and create an **APNs Auth Key (.p8)**; put the .p8 path + key id + team id + topic in the daemon's `config.yaml`. This task wires the app to consume it.

- [ ] **Step 1: Capability + background mode**

In `project.yml`, add the entitlement + capability for the app target:

```yaml
    entitlements:
      path: Sources/MurVoice.entitlements
      properties:
        aps-environment: development   # "production" for release
```

Create `mur-mobile-app/Sources/MurVoice.entitlements` with `aps-environment` accordingly. In `Info.plist`, add:

```xml
    <key>UIBackgroundModes</key>
    <array><string>remote-notification</string></array>
```

- [ ] **Step 2: AppDelegate for registration + silent push**

Create `mur-mobile-app/Sources/AppDelegate.swift`:

```swift
import UIKit

final class AppDelegate: NSObject, UIApplicationDelegate {
    static weak var model: AppModel?

    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        application.registerForRemoteNotifications()
        return true
    }

    func application(_ application: UIApplication,
                     didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        let token = deviceToken.map { String(format: "%02x", $0) }.joined()
        #if DEBUG
        let env = "sandbox"
        #else
        let env = "production"
        #endif
        AppDelegate.model?.registerPush(deviceToken: token, environment: env)
    }

    func application(_ application: UIApplication,
                     didReceiveRemoteNotification userInfo: [AnyHashable: Any],
                     fetchCompletionHandler completionHandler: @escaping (UIBackgroundFetchResult) -> Void) {
        // Silent wake: reconnect + refresh; the payload carries only a channel id.
        let channelId = (userInfo["mur"] as? [String: Any])?["channel_id"] as? String
        AppDelegate.model?.wakeAndRefresh(channelId: channelId) { completionHandler(.newData) }
    }
}
```

- [ ] **Step 3: Wire the AppDelegate + AppModel methods**

In `MurVoiceApp.swift`, attach the delegate: `@UIApplicationDelegateAdaptor(AppDelegate.self) var appDelegate` and set `AppDelegate.model = model` on init/`onAppear`.

In `AppModel.swift` add:

```swift
    func registerPush(deviceToken: String, environment: String) {
        client?.registerPush(deviceToken: deviceToken, environment: environment)
    }
    /// Woken by silent push: ensure connected, refresh the list (and the named
    /// channel if any), then call `done` so iOS knows we fetched.
    func wakeAndRefresh(channelId: String?, done: @escaping () -> Void) {
        // If disconnected, reconnect using the last-known pairing (persisted by
        // the pairing flow), then pull. Best-effort within the background budget.
        start()
        client?.listChannels()
        if let id = channelId { client?.fetchChannelEvents(channelId: id, sinceSeq: nil) }
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { done() }
    }
```

(Reconnect-from-background requires the last pairing to be persisted; if the pairing token is one-time, persist the LAN host + the device's own keypair so the signed reconnect works without re-pairing. Note this as the one real gap to confirm against the pairing flow.)

- [ ] **Step 4: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh   # requires the push capability provisioned
git add mur-mobile-app/project.yml mur-mobile-app/Sources/Info.plist mur-mobile-app/Sources/MurVoice.entitlements mur-mobile-app/Sources/AppDelegate.swift mur-mobile-app/Sources/MurVoiceApp.swift mur-mobile-app/Sources/AppModel.swift
git commit -m "feat(ios): APNs registration + silent-push wake-and-refresh (v4d)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Quality gates + E2E + docs

- [ ] **Step 1: Gates**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-core -p mur-daemon -p mur-mobile-sdk -- -D warnings
cargo nextest run -p mur-common -p mur-core -p mur-mobile-sdk
cd mur-mobile-app && ./build-ios.sh && cd -
```

- [ ] **Step 2: E2E (needs Apple provisioning + a real device)**

```
External setup first: APNs .p8 key + Push capability on run.mur.voice; config.yaml
[mobile.apns] auth_key_p8_path/key_id/team_id/topic/environment set.
1. Pair the phone; confirm the daemon stored the APNs token (paired.json now has
   {"pubkey":…,"apns_token":…,"apns_env":…}).
2. Force-quit the app (disconnect the WebSocket).
3. From the CLI/Hub, touch a channel the owner owns (e.g. trigger a HITL gate).
4. The phone receives a silent push, wakes, reconnects, and the channel list /
   the gated channel are up to date (the HITL card is ready to act on).
5. Confirm NO plaintext is in the push payload (only the channel id).
```

- [ ] **Step 3: Docs + memory**

- `CLAUDE.md` / `mur-mobile-app/README.md`: APNs offline push wakes the phone for channel changes; config `[mobile.apns]`; daemon sends directly (no relay dependency); silent payload only.
- Memory: v4d done (in-repo) — paired-device records + APNs token, daemon-direct silent push to disconnected devices, iOS registration + wake-and-refresh. **External prerequisites: Apple Developer .p8 + Push capability + the `a2` dep.** Optional follow-on: user-visible HITL alert (vs silent), push coalescing, and reconnect-from-cold pairing persistence.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md mur-mobile-app/README.md
git commit -m "docs: APNs offline push (v4d) + external Apple prerequisites

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (`2026-06-16-unified-channel-v4-ios-design.md` §6 v4d):**
- "wake the phone for a HITL gate / channel update while the app is closed" → Task 3 (watcher → APNs wake disconnected devices) + Task 4 (silent-push handler reconnects + fetches). ✓
- "APNs + a relay push path" → realized as **daemon-direct APNs** (outbound HTTPS), which works off-LAN without modifying the external Go relay — a cleaner in-repo path; flagged. ✓
- "silent, reconnect-to-fetch (no plaintext in the relay)" → `silent_push_payload` (content-available + channel id only); Task 4 fetches over the signed tunnel. ✓

**2. Placeholder scan:** No "TBD"/"add later". In-repo cores (paired-record parse, silent payload) are unit-tested; the APNs sender, watcher trigger, frame handler, and iOS wiring are concrete. The **external Apple Developer .p8/capability** and the real APNs send are explicitly called out as out-of-repo prerequisites (not placeholders). The `a2` crate API caveat + the cold-reconnect-pairing-persistence gap are flagged as confirm-on-implement, not hand-waves.

**3. Type consistency:**
- `PairedDevice {pubkey, apns_token, apns_env}` (Task 1) is the unit for `parse_paired`, `set_apns_token`, `paired_devices` (Tasks 1-3).
- `silent_push_payload(channel_id) -> Value` (Task 1) consumed by `Apns::wake` (Task 3) and asserted plaintext-free.
- `ClientFrame::RegisterPush{device_token, environment}` (Task 2) ↔ SDK `register_push(device_token, environment)` ↔ Swift `registerPush(deviceToken:environment:)` (Task 4) — field names align.
- `ApnsConfig{auth_key_p8_path, key_id, team_id, topic, environment}` (Task 3) consumed by `Apns::from_config`.
- `wakeAndRefresh(channelId:done:)` (Task 4) reuses v4a's `listChannels`/`fetchChannelEvents`.

**4. Scope check:** v4d is the in-repo half of offline push — paired records + token, daemon-direct APNs sender + offline trigger, iOS registration + silent wake — across `mur-common`/`mur-core`/`mur-daemon`/`mur-mobile-sdk` + iOS. Unit-tested where pure (records, payload); the network/Apple-provisioned parts are integration + manual, honestly bounded. The external Go relay is untouched; Apple Developer provisioning + the `a2` dep are named external prerequisites. Focused. ✓

No gaps found.
