# Unified Channel v4a — Mobile Sync Foundation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements the **headless sync foundation** from `2026-06-16-unified-channel-v4-ios-design.md` §6 v4a. No Swift UI (that is v4b). Builds on v1 (channel store, shipped). Independent of v3 (renders nothing yet) — it only makes mobile channel-aware.

**Goal:** make the daemon's phone path channel-aware — (1) **persist** every mobile turn into a durable `Channel` (shared with Hub/CLI), and (2) let the phone **pull** the owner's channel list + a channel's events, **subscribe** to live updates while connected, with the Rust SDK exposing the new channel types. All verifiable without the iOS UI.

**Architecture:** mobile turns currently dial the agent and write a transient `~/.mur/agents/<agent>/mobile-events.jsonl` mirror (read by the Hub inbox). v4a **adds** `ChannelService` persistence alongside that mirror (keeps the mirror so the Hub mobile inbox keeps working — a follow-up retires it once the Hub reads channels). The persist + query logic lives in `mur-core/src/mobile.rs` so the **two** daemon paths — `mobile_server.rs::handle_agent_turn` (LAN) and `relay_client.rs::agent_turn` (off-LAN) — share one implementation. A new `ClientFrame::ChannelQuery` / `ServerFrame::ChannelData` pair (authenticated by the paired connection, like the audio frames) carries list/events pulls. Live-push reuses `mur_channel::watch::watch_channels` → a broadcast each connection forwards as a `channel.updated` event. **Sync filter = ownership** (owner == local human); in the single-user trust domain that is every local channel.

**Tech Stack:** Rust — `mur-common` (frame enums), `mur-core` (shared mobile persist/query in `mobile.rs`), `mur-daemon` (LAN + relay wiring, watcher), `mur-mobile-sdk` (UniFFI types + transport). All workspace members. **No Swift UI changes** (the regenerated `mur_mobile_sdk.swift` bindings are a build artifact; the app consuming them is v4b).

**Scope guardrails (from `2026-06-16-unified-channel-v4-ios-design.md` §3, §6, §6.1):**
- **Add** channel persistence; **keep** the `mirror()` write (don't break the Hub mobile inbox). Retiring the mirror is a follow-up.
- **Sync filter = ownership** (single-user ⇒ all local channels). NOT concierge-only, NOT participant-filtered.
- `idempotency_key` left `None` (mobile dedup is not in scope; v3c owns dedup).
- No new dispatch transport — the turn still rides `dial_method`; channel resolution is by agent (`latest_for_agent`/`create_for_agent`), matching the Hub's `persist_exchange`.
- APNs offline push is **out of scope** (v4d). v4a's subscribe is live-while-connected only.

**Key facts locked during exploration (do not re-derive):**
- LAN turn handler: `mur-daemon/src/mobile_server.rs::handle_agent_turn(socket, state, agent, user_text, method, params)` (`:306`) — mirrors user, `dial_method(DialMode::Auto)` (`:328`), mirrors reply, sends `ServerFrame::Event{name:"mobile.reply"}`. `resolve_agent` (`:405`), `mirror` (`:456`), `extract_reply_text` (`:431`).
- Relay turn handler: `mur-daemon/src/relay_client.rs::agent_turn(home, write, agent, user_text, method, params)` (`:255`) — the parallel path; same change applies.
- Frame protocol: `mur-common/src/mobile.rs` — `ClientFrame {Hello, Envelope, AudioStreamStart, AudioChunk, AudioStreamEnd}` (`:26`), `ServerFrame {Paired, Rejected, Event{name,payload}, Transcript, AudioChunk}` (`:52`). Post-pairing frames are authenticated by the paired connection (audio frames carry no per-frame sig).
- SDK: `mur-mobile-sdk/src/lib.rs` — `MobileEvent {Connecting, Connected, Disconnected, Transcript, Reply, Error, AudioChunk}` (`:59`, **no channel types**); `MobileClient` (`:93`) is callback-based (`MobileEventListener::on_event`); `transport.rs` has a `Command` enum (`Send/AudioStreamStart/AudioFrame/AudioStreamEnd/Disconnect`) serialized to `ClientFrame`.
- Shared mobile logic: `mur-core/src/mobile.rs` (`DEFAULT_MOBILE_AGENT`, `ensure_pair_token`, `pairing_uri`). `mur-core` already depends on `mur-channel`.
- The Hub reads the mirror via `mur-hub-gui` `mobile::mobile_events_read` — so the mirror must stay until that is repointed.
- `ChannelService::{open, list (ChannelRow{id,title,state,updated_at}, updated_at-sorted), load_events, latest_for_agent, create_for_agent, append_message, store().load_manifest}`.

---

## File Structure

**Modified:**
- `mur-core/src/mobile.rs` — `persist_mobile_exchange` + `channel_list_json` / `channel_events_json` (shared persist + query helpers).
- `mur-common/src/mobile.rs` — `ClientFrame::ChannelQuery`, `ServerFrame::ChannelData`.
- `mur-daemon/src/mobile_server.rs` — call persist in `handle_agent_turn`; handle `ChannelQuery`; forward live `channel.updated`.
- `mur-daemon/src/relay_client.rs` — call persist in `agent_turn`; handle `ChannelQuery`.
- `mur-daemon/src/main.rs` (or where the daemon spawns long-lived tasks) — spawn the channel watcher → broadcast.
- `mur-mobile-sdk/src/lib.rs` — `MobileEvent::{ChannelList, ChannelEvents, ChannelUpdate}` + `ChannelListItem`/`ChannelEventItem` records + `MobileClient::{list_channels, fetch_channel_events}`.
- `mur-mobile-sdk/src/transport.rs` — `Command::ChannelQuery`; inbound `ServerFrame::ChannelData`/`channel.updated` → `MobileEvent`.

No new files. No Swift source changes (bindings regenerate).

---

## Task 1: Persist mobile turns into a Channel

**Files:**
- Modify: `mur-core/src/mobile.rs`; `mur-daemon/src/mobile_server.rs:332-343`; `mur-daemon/src/relay_client.rs` (`agent_turn`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `mur-core/src/mobile.rs` (find/append with `grep -n "#\[cfg(test)\]" mur-core/src/mobile.rs`):

```rust
    #[test]
    fn persist_mobile_exchange_writes_both_turns_to_one_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(tmp.path(), "mur", "what's my schedule?", "you have 2 meetings");
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let id = svc.latest_for_agent("mur").unwrap().expect("channel created");
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "what's my schedule?");
        assert_eq!(evs[1].payload["text"], "you have 2 meetings");
        // Second exchange appends to the SAME channel (shared, like the Hub).
        persist_mobile_exchange(tmp.path(), "mur", "and tomorrow?", "3 meetings");
        assert_eq!(svc.list(10).unwrap().len(), 1);
        assert_eq!(svc.load_events(&id).unwrap().len(), 4);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core mobile::tests::persist_mobile_exchange_writes_both_turns_to_one_channel` — Expected: FAIL, function not found.

- [ ] **Step 3: Implement the persist helper**

In `mur-core/src/mobile.rs`, add (with `use mur_channel::ChannelService;` and `use mur_common::channel::{ChannelActor, EventKind};` at the top):

```rust
/// Persist one mobile user→agent exchange into the agent's channel (resolved
/// once), so phone conversations are durable and shared with the Hub/CLI. Best-
/// effort: failures are logged, never surfaced to the phone. Mirrors the Hub's
/// `chat::persist_exchange`. The channel is created on the first real exchange.
pub fn persist_mobile_exchange(home: &std::path::Path, agent: &str, user_text: &str, agent_text: &str) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = match svc.latest_for_agent(agent)? {
            Some(id) => id,
            None => svc.create_for_agent(agent)?.id,
        };
        svc.append_message(&id, ChannelActor::local_human(), EventKind::Message, user_text, None)?;
        svc.append_message(
            &id,
            ChannelActor::Agent { id: agent.to_string() },
            EventKind::Message,
            agent_text,
            None,
        )?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("mobile channel persist failed for {agent}: {e:#}");
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core mobile::tests::persist_mobile_exchange_writes_both_turns_to_one_channel` — Expected: PASS.

- [ ] **Step 5: Call it from both daemon turn paths (keep the mirror)**

In `mur-daemon/src/mobile_server.rs::handle_agent_turn`, after `reply_text` is computed and the reply mirror is written (`:343`), before sending the reply frame, add (only persist non-error replies):

```rust
    if !reply_text.starts_with("[error]") {
        mur_core::mobile::persist_mobile_exchange(
            state.mur_home.as_path(),
            agent,
            user_text,
            &reply_text,
        );
    }
```

In `mur-daemon/src/relay_client.rs::agent_turn`, find the equivalent point (after it computes the reply text, before/after its reply mirror) and add the same call with that fn's `home` + `agent` + `user_text` + reply text. (Mirror the LAN path exactly; the `mirror()` writes stay.)

- [ ] **Step 6: Build + commit**

```bash
cargo build -p mur-core -p mur-daemon
git add mur-core/src/mobile.rs mur-daemon/src/mobile_server.rs mur-daemon/src/relay_client.rs
git commit -m "feat(mobile): persist phone turns into a durable Channel (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: Channel-query protocol + handler

**Files:**
- Modify: `mur-common/src/mobile.rs`; `mur-core/src/mobile.rs`

- [ ] **Step 1: Write the failing tests**

Add to `mur-common/src/mobile.rs` tests (append a `#[cfg(test)] mod tests` if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_query_and_data_frames_round_trip() {
        let q = ClientFrame::ChannelQuery {
            op: "events".into(),
            channel_id: Some("c1".into()),
            since_seq: Some(3),
        };
        let s = serde_json::to_string(&q).unwrap();
        assert!(s.contains("\"type\":\"channel_query\""));
        let back: ClientFrame = serde_json::from_str(&s).unwrap();
        matches!(back, ClientFrame::ChannelQuery { .. });

        let d = ServerFrame::ChannelData { op: "list".into(), payload: serde_json::json!([]) };
        let s2 = serde_json::to_string(&d).unwrap();
        assert!(s2.contains("\"type\":\"channel_data\""));
    }
}
```

Add to `mur-core/src/mobile.rs` tests:

```rust
    #[test]
    fn channel_query_list_and_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(tmp.path(), "mur", "hi", "hello");
        // list → one summary with the agent + a turn count.
        let list = channel_query(tmp.path(), "list", None, None).unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["agents"][0], "mur");
        assert!(arr[0]["turns"].as_u64().unwrap() >= 2);
        let cid = arr[0]["id"].as_str().unwrap().to_string();
        // events → the two messages; since_seq filters.
        let evs = channel_query(tmp.path(), "events", Some(cid.clone()), None).unwrap();
        assert_eq!(evs.as_array().unwrap().len(), 2);
        let evs1 = channel_query(tmp.path(), "events", Some(cid), Some(1)).unwrap();
        assert_eq!(evs1.as_array().unwrap().len(), 1);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common mobile::tests::channel_query_and_data_frames_round_trip` and `cargo test -p mur-core mobile::tests::channel_query_list_and_events` — Expected: FAIL (no variants / no `channel_query`).

- [ ] **Step 3: Add the frame variants**

In `mur-common/src/mobile.rs`, add to `ClientFrame` (after `AudioStreamEnd`):

```rust
    /// Pull channel data. `op` ∈ "list" | "events". For "events", `channel_id`
    /// is required and `since_seq` (inclusive) enables catch-up. Authenticated
    /// by the paired connection (like the audio frames).
    ChannelQuery {
        op: String,
        #[serde(default)]
        channel_id: Option<String>,
        #[serde(default)]
        since_seq: Option<u64>,
    },
```

Add to `ServerFrame` (after `AudioChunk`):

```rust
    /// Response to a `ChannelQuery`. `op` echoes the request; `payload` is a JSON
    /// array (channel summaries for "list", events for "events").
    ChannelData { op: String, payload: serde_json::Value },
```

- [ ] **Step 4: Add the query helper**

In `mur-core/src/mobile.rs`, add (single-user ⇒ owner == local human ⇒ every local channel is in scope; a multi-user owner filter is a future add):

```rust
/// Max channels returned to the phone (v4 scale is small).
const MOBILE_CHANNEL_LIMIT: usize = 200;

/// Serve a channel pull for the phone. `op` ∈ "list" | "events".
/// "list" → array of `{id,title,state,goal,updated_at,agents,turns}` (newest
/// first, empties hidden). "events" → that channel's events at/after `since_seq`.
/// Ownership filter: single-user, so all local channels are the owner's.
pub fn channel_query(
    home: &std::path::Path,
    op: &str,
    channel_id: Option<String>,
    since_seq: Option<u64>,
) -> anyhow::Result<serde_json::Value> {
    let svc = ChannelService::open(home)?;
    match op {
        "list" => {
            let mut out = Vec::new();
            for row in svc.list(MOBILE_CHANNEL_LIMIT)? {
                let events = svc.load_events(&row.id).unwrap_or_default();
                if events.is_empty() {
                    continue;
                }
                let manifest = svc.store().load_manifest(&row.id).ok();
                let agents: Vec<String> = manifest
                    .as_ref()
                    .map(|m| {
                        m.participants
                            .iter()
                            .filter_map(|p| match &p.actor {
                                ChannelActor::Agent { id } => Some(id.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let goal = manifest.as_ref().map(|m| m.goal.statement.clone()).unwrap_or_default();
                out.push(serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "state": row.state,
                    "goal": goal,
                    "updated_at": row.updated_at,
                    "agents": agents,
                    "turns": events.len(),
                }));
            }
            Ok(serde_json::Value::Array(out))
        }
        "events" => {
            let id = channel_id.ok_or_else(|| anyhow::anyhow!("events query needs channel_id"))?;
            let evs: Vec<_> = svc
                .load_events(&id)?
                .into_iter()
                .filter(|e| since_seq.is_none_or(|s| e.seq >= s))
                .collect();
            Ok(serde_json::to_value(evs)?)
        }
        other => anyhow::bail!("unknown channel query op `{other}`"),
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mur-common mobile:: && cargo test -p mur-core mobile::` — Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/mobile.rs mur-core/src/mobile.rs
git commit -m "feat(mobile): ChannelQuery/ChannelData protocol + channel_query helper (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: Wire `ChannelQuery` into both daemon paths

**Files:**
- Modify: `mur-daemon/src/mobile_server.rs` (`handle_socket` match), `mur-daemon/src/relay_client.rs` (`handle_frame` match)

- [ ] **Step 1: Handle the frame in the LAN server**

In `mobile_server.rs::handle_socket`, add a match arm alongside the other `ClientFrame::` arms (e.g. after `AudioStreamEnd`):

```rust
            ClientFrame::ChannelQuery { op, channel_id, since_seq } => {
                let payload = mur_core::mobile::channel_query(
                    state.mur_home.as_path(),
                    &op,
                    channel_id,
                    since_seq,
                )
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
                if send_frame(&mut socket, &ServerFrame::ChannelData { op, payload })
                    .await
                    .is_err()
                {
                    break;
                }
            }
```

- [ ] **Step 2: Handle the frame in the relay path**

In `relay_client.rs::handle_frame` (the `match frame { … }` over `ClientFrame`), add the parallel arm using `relay_send` and the `home` in scope:

```rust
        ClientFrame::ChannelQuery { op, channel_id, since_seq } => {
            let payload = mur_core::mobile::channel_query(home, &op, channel_id, since_seq)
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }));
            relay_send(write, &ServerFrame::ChannelData { op, payload }).await?;
        }
```

- [ ] **Step 3: Build**

Run: `cargo build -p mur-daemon` — Expected: compiles. (Both match statements are now exhaustive over the new `ClientFrame::ChannelQuery`.)

- [ ] **Step 4: Commit**

```bash
git add mur-daemon/src/mobile_server.rs mur-daemon/src/relay_client.rs
git commit -m "feat(mobile): serve ChannelQuery over LAN + relay (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: SDK channel types + pull methods

**Files:**
- Modify: `mur-mobile-sdk/src/lib.rs`, `mur-mobile-sdk/src/transport.rs`

> SDK methods are FFI/network glue (no pure logic to unit-test); verified by `cargo build` + the regenerated bindings + the manual E2E in Task 6. Keep them thin — the daemon (Task 2, unit-tested) owns the logic.

- [ ] **Step 1: Add the UniFFI records + events**

In `lib.rs`, add records near `MobileConfig` (`:44`):

```rust
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelListItem {
    pub id: String,
    pub title: String,
    pub state: String,
    pub goal: String,
    pub updated_at: String,
    pub agents: Vec<String>,
    pub turns: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelEventItem {
    pub seq: u64,
    pub ts: String,
    pub actor_kind: String, // "human" | "agent" | "system"
    pub actor_name: String, // agent id / human name; "" for system
    pub kind: String,       // "message" | "state-change" | …
    pub text: String,       // flattened payload.text
}
```

Add `MobileEvent` variants (`:84`):

```rust
    /// Result of `list_channels()` — the owner's channels, newest first.
    ChannelList { channels: Vec<ChannelListItem> },
    /// Result of `fetch_channel_events()` for `channel_id`.
    ChannelEvents { channel_id: String, events: Vec<ChannelEventItem> },
    /// A channel changed while connected (live push); pull to refresh.
    ChannelUpdate { channel_id: String },
```

- [ ] **Step 2: Add the `Command` + client methods**

In `transport.rs`, add to the `Command` enum: `ChannelQuery { op: String, channel_id: Option<String>, since_seq: Option<u64> }`, and in the send loop serialize it to `ClientFrame::ChannelQuery { … }`. In the inbound `ServerFrame` match, handle `ServerFrame::ChannelData { op, payload }` by parsing `payload` into the records and emitting `MobileEvent::ChannelList`/`ChannelEvents` (by `op`), and handle a `ServerFrame::Event { name: "channel.updated", payload }` by emitting `MobileEvent::ChannelUpdate { channel_id: payload["channel_id"] }`.

In `lib.rs`, add `MobileClient` methods (mirror `send_text`'s `cmd_tx` send pattern):

```rust
    /// Request the owner's channel list (result arrives as MobileEvent::ChannelList).
    pub fn list_channels(&self) -> Result<(), SdkError> {
        self.send_cmd(Command::ChannelQuery { op: "list".into(), channel_id: None, since_seq: None })
    }
    /// Request a channel's events (result arrives as MobileEvent::ChannelEvents).
    pub fn fetch_channel_events(&self, channel_id: String, since_seq: Option<u64>) -> Result<(), SdkError> {
        self.send_cmd(Command::ChannelQuery { op: "events".into(), channel_id: Some(channel_id), since_seq })
    }
```

(Use the existing private command-send helper — `grep -n "cmd_tx\|fn send_cmd\|send_text" mur-mobile-sdk/src/lib.rs` — and mirror its error handling.)

- [ ] **Step 3: Build + regenerate bindings**

Run:
```bash
cargo build -p mur-mobile-sdk
# regenerate the Swift bindings the app consumes (mirror the existing gen step):
grep -rn "uniffi-bindgen\|generate.*swift\|mur_mobile_sdk.swift" mur-mobile-sdk/ mur-mobile-app/ build*.sh 2>/dev/null
# run that generator so mur-mobile-app/Generated/mur_mobile_sdk.swift picks up the new types
```
Expected: SDK compiles; bindings regenerate with `ChannelListItem`/`ChannelEventItem`/the new `MobileEvent` cases. (No Swift source edits — the app wiring is v4b.)

- [ ] **Step 4: Commit**

```bash
git add mur-mobile-sdk/src/lib.rs mur-mobile-sdk/src/transport.rs mur-mobile-app/Generated/mur_mobile_sdk.swift
git commit -m "feat(mobile-sdk): channel list/events types + pull methods (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Live-push (subscribe while connected)

**Files:**
- Modify: `mur-daemon/src/main.rs` (or the daemon's task-spawn site), `mur-daemon/src/mobile_server.rs` (`handle_socket` recv loop)

> This is the integration-heavy capstone. If scope must be trimmed, ship Tasks 1-4 first (pull works fully); Task 5 (live push) can land as v4a-2. The pull catch-up (`fetch_channel_events` since last-seen `seq`) already covers correctness; this task is the latency win.

- [ ] **Step 1: Broadcast channel changes**

Where the daemon spawns long-lived tasks (`main.rs`, near the mobile server / relay spawn), create a `tokio::sync::broadcast::channel::<String>(256)` and start the watcher (reuse the Hub's pattern):

```rust
    let (chan_tx, _chan_rx) = tokio::sync::broadcast::channel::<String>(256);
    {
        let tx = chan_tx.clone();
        let home = mur_home.clone();
        std::thread::spawn(move || {
            match mur_channel::watch::watch_channels(&home, move |channel_id| {
                let _ = tx.send(channel_id);
            }) {
                Ok(w) => { std::mem::forget(w); } // keep alive for process lifetime
                Err(e) => tracing::warn!("mobile channel watcher failed: {e:#}"),
            }
        });
    }
```

Thread `chan_tx` into `MobileState` (so each `handle_socket` can `subscribe()`).

- [ ] **Step 2: Forward changes on the connected socket**

In `handle_socket`, after pairing, subscribe and select between incoming frames and channel-change broadcasts. Replace the bare `recv_text` loop with a `tokio::select!` that also drains `chan_rx`:

```rust
    let mut chan_rx = state.chan_tx.subscribe();
    loop {
        tokio::select! {
            maybe = recv_text(&mut socket) => {
                let Some(txt) = maybe else { break };
                // … existing frame-parse + handle (Hello/Envelope/Audio*/ChannelQuery) …
            }
            Ok(channel_id) = chan_rx.recv() => {
                if send_frame(&mut socket, &ServerFrame::Event {
                    name: "channel.updated".into(),
                    payload: serde_json::json!({ "channel_id": channel_id }),
                }).await.is_err() {
                    break;
                }
            }
        }
    }
```

(Restructure the existing `while let Some(txt) = recv_text(...)` body into the first select arm; keep all current arms intact.)

- [ ] **Step 3: Build + commit**

```bash
cargo build -p mur-daemon
git add mur-daemon/src/main.rs mur-daemon/src/mobile_server.rs
git commit -m "feat(mobile): live channel.updated push while connected (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 6: Quality gates + E2E + docs

- [ ] **Step 1: Format / clippy / tests**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-core -p mur-daemon -p mur-mobile-sdk -- -D warnings
cargo nextest run -p mur-common -p mur-core -p mur-channel -p mur-mobile-sdk
```
Expected: clean + green (ignore the 4 pre-existing `conversations::summarize::rollup` failures).

- [ ] **Step 2: Headless E2E (no phone needed)**

```bash
# 1. Send a mobile-style turn (or run the existing mobile test harness) so a turn persists.
# 2. Confirm a channel now holds the exchange:
ls ~/.mur/channels && cat ~/.mur/channels/*/events.jsonl | tail -4
# expect a human Message + an Agent{mur} Message.
# 3. Confirm the same channel shows up in the Hub Work view (v2) and in `mur agent cli mur` /channels.
# 4. (If a paired phone / the mobile test client is available) send ClientFrame::ChannelQuery{op:"list"}
#    and assert a ServerFrame::ChannelData with the channel summary comes back; touch the channel from
#    the CLI and assert a `channel.updated` event arrives on the connected socket.
```

- [ ] **Step 3: Docs**

- `CLAUDE.md`: note that mobile turns now persist to Channels and the daemon serves `ChannelQuery` (list/events) + `channel.updated` push (v4a). One line.
- Note the mirror (`mobile-events.jsonl`) is retained for the Hub mobile inbox; retiring it is a follow-up.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mobile channel sync foundation (v4a)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (against `2026-06-16-unified-channel-v4-ios-design.md` §6 v4a + §6.1):**
- "mobile turns persist through ChannelService (replace the transient write … with append_message)" → Task 1. **Deviation, flagged:** v4a *adds* persistence and *keeps* the `mirror()` write (the Hub `mobile_events_read` still reads it); retiring the mirror is a follow-up. This is the non-breaking engineering call. ✓
- "channels/list, channels/events (since seq N), channels/subscribe RPCs" → Task 2 (`list`/`events` via `ChannelQuery`) + Task 5 (`subscribe`/live-push). ✓
- "SDK channel types + MobileEvent" → Task 4 (`ChannelListItem`/`ChannelEventItem` + `ChannelList`/`ChannelEvents`/`ChannelUpdate`). ✓
- "live-push-while-connected; APNs deferred" → Task 5 (broadcast + per-socket forward); APNs explicitly out of scope. ✓
- "sync filter = ownership (all local channels), not concierge-only, not participant" → Task 2 `channel_query("list")` returns all local channels (single-user owner). ✓
- "no new dispatch transport" → Task 1 resolves the channel by agent and rides the existing `dial_method`. ✓
- "idempotency_key None (v3c owns dedup)" → persist passes `None`. ✓

**2. Placeholder scan:** No "TBD"/"handle errors"/"similar to". Tasks 4-5 reference a `grep` to locate the bindings-generator and the private `send_cmd` helper / the recv-loop body to restructure — each names exactly what to find and the exact code to add. Tasks 4-5 are honestly flagged as build/integration-verified (FFI + live socket), with the unit-tested daemon logic (Tasks 1-2) owning correctness.

**3. Type consistency:**
- `persist_mobile_exchange(home, agent, user_text, agent_text)` (Task 1) called identically in both daemon paths (Task 1 Step 5).
- `channel_query(home, op, channel_id: Option<String>, since_seq: Option<u64>) -> Value` (Task 2) called identically in both daemon frame handlers (Task 3).
- `ClientFrame::ChannelQuery {op, channel_id, since_seq}` / `ServerFrame::ChannelData {op, payload}` (Task 2) consumed in Task 3 (daemon) and Task 4 (SDK transport).
- SDK `MobileEvent::{ChannelList{channels}, ChannelEvents{channel_id,events}, ChannelUpdate{channel_id}}` + `ChannelListItem`/`ChannelEventItem` (Task 4) — the `channel.updated` event name matches between Task 5 (daemon emit) and Task 4 (SDK parse).
- The "list" summary keys (`id,title,state,goal,updated_at,agents,turns`) match between `channel_query` (Task 2) and `ChannelListItem` (Task 4).

**4. Scope check:** Single sub-project (v4a), headless (no Swift source), 6 tasks across 4 workspace crates. The unit-testable core (persist + query + frame serde) is TDD'd; the FFI + live-socket wiring is build/integration-verified with an explicit trim point (Tasks 1-4 ship pull; Task 5 live-push can be v4a-2). v4b (UI), v4c (@mention), v4d (APNs) are correctly out of scope. ✓

No gaps found.
