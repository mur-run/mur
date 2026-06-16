# Unified Channel v4c — Drop-Into-Channel, @mention & Phone HITL Respond — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements `2026-06-16-unified-channel-v4-ios-design.md` §6 v4c (research-gated). **Depends on v4a** (mobile sync + signed-frame transport), **v4b** (the Channel-list home + read-only channel-detail), and the **v3c gate** + **v3d signing** (for authoritative HITL respond). All planned/partly-built — wire against their real interfaces.

**Goal:** turn the phone from a launcher-plus-viewer into a full Channel participant: (1) **send into any channel** (drop into a Hub/CLI-originated channel, not just the concierge), (2) **@mention** a specialist as a scoping hint to the concierge, and (3) **respond to a HITL gate from the phone** — the killer mobile job — authoritatively, now that v3d makes the channel `HitlResponse` trustworthy.

**Architecture:** the phone keeps addressing through the local daemon (no new transport): `send_text` gains a `channel_id` so a turn lands in a specific channel (the daemon persists into it and dials its router agent, still `mur`). `@mention` is a **client-side scoping hint** — the literal `@name …` text is delivered to the concierge channel (the concierge's v3b decomposition may honor it by delegating); autocomplete sources from the channel's participants + known agents; it **never** opens a phone→specialist socket. **Phone HITL respond** rides the already-signed frame path: the phone sends a `HitlRespond` frame; the daemon (which verifies every frame is from a **paired** device) writes a **v3d-signed** `HitlResponse` event *as the trusted local writer on behalf of the paired phone*, which the waiting v3c gate verifies and releases — so the phone's approval is authoritative without the phone itself being a channel writer.

**Tech Stack:** Rust (`mur-mobile-sdk` send signature; `mur-common` frame; `mur-daemon` persist + HITL respond handler) + SwiftUI (`mur-mobile-app`: detail send affordance, @mention autocomplete, HITL respond buttons). Builds on v4a/v4b/v3c/v3d.

**Scope guardrails (from the v4 spec §3, §4.2, §6 v4c, §7):**
- `@mention` is **advisory to the orchestrator, never authoritative to a worker**; it never opens a second socket. The phone sends the hint to the concierge channel; the concierge (v3b) decides.
- Sending into a channel still dials that channel's **router agent** (`mur`) — the phone never addresses a specialist directly.
- Phone HITL respond is **authoritative only because v3d signing is in place** (the daemon writes a signed `HitlResponse`; the gate verifies). High-risk gates are gated by the same v3c risk tiers — the phone can respond to any tier the gate accepts a channel response for.
- Research-gated: ship behind the funnel proving out in v4b (the v4 spec parks v4c until then).

**Key facts locked during exploration (do not re-derive):**
- `mur-mobile-sdk/src/lib.rs::send_text(&self, text: String)` (`:217`) carries **no** `channel_id`; `transport.rs` `Command::Send` wraps a signed A2A envelope.
- Daemon: `mobile_server.rs::handle_agent_turn(socket, state, agent, user_text, method, params)` (`:306`) → `dial_method` + `mur_core::mobile::persist_mobile_exchange` (v4a, resolves channel by `latest_for_agent`). `relay_client.rs::agent_turn` is the parallel path. Every `ClientFrame::Envelope` is Ed25519-verified against the **paired** pubkey (`mobile_server.rs:177-202`).
- v3c gate (`mur-core/src/hitl/gate.rs::wait_for_response`) tails the log for a `HitlResponse{hitl_id, action_hash, allow}` and (v3d) verifies its sig before releasing.
- v3d: `ChannelService::append_signed(channel_id, identity, kv, actor, kind, payload, idem)` + `mur_core` `append_as_writer` (loads the router identity, signs). `HitlResponse` payload (v3c) = `{hitl_id, action_hash, allow, reason, surface}`.
- v4b: `ChannelDetailView` (read-only), `AppModel.{openChannel, detailEvents}`, the `hitlCard` (display-only "respond on desktop").

---

## File Structure

**Modified:**
- `mur-mobile-sdk/src/lib.rs` — `send_text` gains `channel_id: Option<String>`; new `hitl_respond(channel_id, hitl_id, allow, reason)`.
- `mur-mobile-sdk/src/transport.rs` — thread `channel_id`; new `Command::HitlRespond` → `ClientFrame::HitlRespond`.
- `mur-common/src/mobile.rs` — `ClientFrame::HitlRespond {channel_id, hitl_id, allow, reason}`.
- `mur-core/src/mobile.rs` — `persist_mobile_exchange` honors an explicit `channel_id`; new `respond_hitl(home, channel_id, hitl_id, allow, reason)` (writes a signed `HitlResponse` via `append_as_writer`).
- `mur-daemon/src/mobile_server.rs` + `relay_client.rs` — thread `channel_id` from params; handle `ClientFrame::HitlRespond`.
- `mur-mobile-app/Sources/AppModel.swift` — `sendToChannel`, `respondHitl`, `@mention` state.
- `mur-mobile-app/Sources/ChannelDetailView.swift` — send affordance + HITL respond buttons.
- `mur-mobile-app/Sources/ChannelFormatting.swift` — `@mention` parse/autocomplete helpers.

---

## Task 1: `channel_id` on the send path (Rust)

**Files:**
- Modify: `mur-core/src/mobile.rs`; `mur-mobile-sdk/src/lib.rs`,`transport.rs`; `mur-daemon/src/mobile_server.rs`,`relay_client.rs`

- [ ] **Step 1: Write the failing test**

Add to `mur-core/src/mobile.rs` tests:

```rust
    #[test]
    fn persist_into_explicit_channel_targets_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        // Two channels for the same agent; an explicit id must target the older one.
        let a = svc.create_for_agent("mur").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let _b = svc.create_for_agent("mur").unwrap(); // newer = latest_for_agent
        persist_mobile_exchange_into(tmp.path(), "mur", Some(&a.id), "q", "ans");
        assert_eq!(svc.load_events(&a.id).unwrap().len(), 2, "explicit id targeted");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core mobile::tests::persist_into_explicit_channel_targets_it` — Expected: FAIL (`persist_mobile_exchange_into` not found).

- [ ] **Step 3: Add the explicit-channel persist**

In `mur-core/src/mobile.rs`, refactor `persist_mobile_exchange` to delegate to a channel-explicit variant:

```rust
/// Persist a mobile exchange into `channel_id` if given, else the agent's latest
/// (or a fresh) channel. Signed via the router identity (v3d) when available.
pub fn persist_mobile_exchange_into(home: &std::path::Path, agent: &str, channel_id: Option<&str>, user_text: &str, agent_text: &str) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = match channel_id {
            Some(id) => id.to_string(),
            None => match svc.latest_for_agent(agent)? { Some(id) => id, None => svc.create_for_agent(agent)?.id },
        };
        crate::channel_writer::append_as_writer(&svc, home, &id, agent, ChannelActor::local_human(), EventKind::Message, serde_json::json!({"text": user_text}), None)?;
        crate::channel_writer::append_as_writer(&svc, home, &id, agent, ChannelActor::Agent { id: agent.to_string() }, EventKind::Message, serde_json::json!({"text": agent_text}), None)?;
        Ok(())
    })();
    if let Err(e) = res { tracing::warn!("mobile channel persist failed: {e:#}"); }
}

pub fn persist_mobile_exchange(home: &std::path::Path, agent: &str, user_text: &str, agent_text: &str) {
    persist_mobile_exchange_into(home, agent, None, user_text, agent_text)
}
```

- [ ] **Step 4: Thread `channel_id` through the SDK + daemon**

- `mur-mobile-sdk/src/lib.rs`: `pub fn send_text(&self, text: String, channel_id: Option<String>)` → put `channel_id` in the A2A params (`params["channel_id"] = …` in the envelope builder). (`grep -n "fn send_text\|sign_agent_send\|params" mur-mobile-sdk/src/lib.rs` for the builder.)
- `mur-daemon/src/mobile_server.rs`: in the `Envelope` handler, extract `channel_id` from `params` and pass it to `handle_agent_turn`, which calls `persist_mobile_exchange_into(home, agent, channel_id.as_deref(), user_text, &reply_text)`. Mirror in `relay_client.rs::agent_turn`.

- [ ] **Step 5: Build + test + commit**

Run: `cargo build -p mur-core -p mur-daemon -p mur-mobile-sdk && cargo test -p mur-core mobile::` — Expected: PASS.

```bash
git add mur-core/src/mobile.rs mur-mobile-sdk/src/lib.rs mur-mobile-sdk/src/transport.rs mur-daemon/src/mobile_server.rs mur-daemon/src/relay_client.rs
git commit -m "feat(mobile): channel_id-on-send — drop a turn into a specific channel (v4c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: Phone HITL respond (Rust)

**Files:**
- Modify: `mur-common/src/mobile.rs`; `mur-core/src/mobile.rs`; `mur-daemon/src/mobile_server.rs`,`relay_client.rs`; SDK `lib.rs`,`transport.rs`

- [ ] **Step 1: Write the failing test**

Add to `mur-core/src/mobile.rs` tests:

```rust
    #[test]
    fn respond_hitl_writes_a_hitl_response_event() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        // A pending HitlRequest must exist so we can echo its action_hash.
        svc.append(&ch.id, ChannelActor::System, EventKind::HitlRequest,
            serde_json::json!({"hitl_id":"h1","action_hash":"AH","tier":"destructive","summary":"rm"}), None).unwrap();
        respond_hitl(tmp.path(), "mur", &ch.id, "h1", true, "ok from phone");
        let resp = svc.load_events(&ch.id).unwrap().into_iter()
            .find(|e| e.kind == EventKind::HitlResponse).expect("response written");
        assert_eq!(resp.payload["hitl_id"], "h1");
        assert_eq!(resp.payload["action_hash"], "AH", "echoes the request's hash");
        assert_eq!(resp.payload["allow"], true);
        assert_eq!(resp.payload["surface"], "ios");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core mobile::tests::respond_hitl_writes_a_hitl_response_event` — Expected: FAIL.

- [ ] **Step 3: Implement `respond_hitl`**

In `mur-core/src/mobile.rs` (reuses the v3c `HitlRequest`/`HitlResponse` types + v3d `append_as_writer`):

```rust
/// Write a (v3d-signed) HitlResponse on behalf of a paired phone. The daemon has
/// already verified the frame came from a paired device, so the local writer
/// (router identity) records the authoritative response the v3c gate is waiting on.
pub fn respond_hitl(home: &std::path::Path, agent: &str, channel_id: &str, hitl_id: &str, allow: bool, reason: &str) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        // Echo the pending request's action_hash so the gate's hash check passes.
        let action_hash = svc.load_events(channel_id)?.iter().rev()
            .filter(|e| e.kind == EventKind::HitlRequest)
            .find_map(|e| {
                let p = &e.payload;
                (p.get("hitl_id").and_then(|v| v.as_str()) == Some(hitl_id))
                    .then(|| p.get("action_hash").and_then(|v| v.as_str()).unwrap_or("").to_string())
            })
            .ok_or_else(|| anyhow::anyhow!("no pending HitlRequest {hitl_id}"))?;
        let payload = serde_json::json!({
            "hitl_id": hitl_id, "action_hash": action_hash,
            "allow": allow, "reason": reason, "surface": "ios",
        });
        crate::channel_writer::append_as_writer(&svc, home, channel_id, agent,
            ChannelActor::local_human(), EventKind::HitlResponse, payload, None)?;
        Ok(())
    })();
    if let Err(e) = res { tracing::warn!("mobile hitl respond failed: {e:#}"); }
}
```

- [ ] **Step 4: Frame + handlers + SDK method**

- `mur-common/src/mobile.rs`: `ClientFrame::HitlRespond { channel_id: String, hitl_id: String, allow: bool, #[serde(default)] reason: String }`.
- `mobile_server.rs` + `relay_client.rs`: handle it → `mur_core::mobile::respond_hitl(home, "mur", &channel_id, &hitl_id, allow, &reason)`; reply a `ServerFrame::Event{name:"hitl.ack"}`.
- SDK `lib.rs`: `pub fn hitl_respond(&self, channel_id: String, hitl_id: String, allow: bool, reason: String) -> Result<(), SdkError>` → `Command::HitlRespond`; `transport.rs` serializes to `ClientFrame::HitlRespond`.

- [ ] **Step 5: Build + commit**

Run: `cargo build -p mur-core -p mur-daemon -p mur-mobile-sdk && cargo test -p mur-core mobile::` — Expected: PASS.

```bash
git add mur-common/src/mobile.rs mur-core/src/mobile.rs mur-daemon/src/mobile_server.rs mur-daemon/src/relay_client.rs mur-mobile-sdk/src/lib.rs mur-mobile-sdk/src/transport.rs
git commit -m "feat(mobile): phone HITL respond — daemon writes signed HitlResponse (v4c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: @mention parsing + autocomplete helpers (Swift)

**Files:**
- Modify: `mur-mobile-app/Sources/ChannelFormatting.swift`

- [ ] **Step 1: Add the pure helpers** (no XCTest target → build + manual; pure funcs for review/future tests)

```swift
/// Parse a trailing "@partial" token from the draft for autocomplete. Returns
/// the partial (without "@") if the cursor is in a mention token, else nil.
func mentionToken(in draft: String) -> String? {
    guard let at = draft.lastIndex(of: "@") else { return nil }
    let after = draft[draft.index(after: at)...]
    // A mention token has no whitespace after the "@".
    return after.contains(where: { $0.isWhitespace }) ? nil : String(after)
}

/// Autocomplete candidates for a partial @mention: channel participants first,
/// then other known agents, filtered by prefix, deduped.
func mentionCandidates(partial: String, participants: [String], knownAgents: [String]) -> [String] {
    let p = partial.lowercased()
    var seen = Set<String>(); var out: [String] = []
    for name in participants + knownAgents where name.lowercased().hasPrefix(p) {
        if seen.insert(name).inserted { out.append(name) }
    }
    return out
}

/// Replace the trailing "@partial" with "@chosen " in the draft.
func applyMention(_ draft: String, choosing name: String) -> String {
    guard let at = draft.lastIndex(of: "@") else { return draft }
    return String(draft[..<at]) + "@" + name + " "
}
```

- [ ] **Step 2: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh
git add mur-mobile-app/Sources/ChannelFormatting.swift
git commit -m "feat(ios): @mention parse + autocomplete helpers (v4c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: Channel-detail send + @mention bar + HITL respond (Swift)

**Files:**
- Modify: `mur-mobile-app/Sources/AppModel.swift`, `ChannelDetailView.swift`

- [ ] **Step 1: AppModel methods**

In `AppModel.swift` add:

```swift
    func sendToChannel(_ text: String, channelId: String) {
        let t = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { return }
        // Optimistic local echo into the open detail feed.
        client?.sendText(text: t, channelId: channelId)
    }
    func respondHitl(channelId: String, hitlId: String, allow: Bool) {
        client?.hitlRespond(channelId: channelId, hitlId: hitlId, allow: allow, reason: allow ? "approved on phone" : "denied on phone")
    }
    /// Agents known locally + the open channel's participants, for @mention.
    var mentionableAgents: [String] {
        var set = channels.first(where: { $0.id == detailChannelId })?.agents ?? []
        for c in channels { for a in c.agents where !set.contains(a) { set.append(a) } }
        return set
    }
```

(`send_text`'s new `channel_id` arg → the generated Swift is `sendText(text:channelId:)`; match the binding.)

- [ ] **Step 2: Detail send affordance + @mention autocomplete**

In `ChannelDetailView`, add a bottom `safeAreaInset` with a `TextField` + send button that calls `model.sendToChannel(draft, channelId: channelId)`, and an autocomplete strip driven by `mentionToken`/`mentionCandidates`:

```swift
    @State private var draft = ""
    // … inside body, after the ScrollView … 
    .safeAreaInset(edge: .bottom) {
        VStack(spacing: 4) {
            if let partial = mentionToken(in: draft) {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack {
                        ForEach(mentionCandidates(partial: partial, participants: model.detailParticipants, knownAgents: model.mentionableAgents), id: \.self) { name in
                            Button("@\(name)") { draft = applyMention(draft, choosing: name) }
                                .font(.caption).buttonStyle(.bordered)
                        }
                    }.padding(.horizontal)
                }
            }
            HStack {
                TextField("Message this channel…", text: $draft)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { model.sendToChannel(draft, channelId: channelId); draft = "" }
                Button { model.sendToChannel(draft, channelId: channelId); draft = "" }
                    label: { Image(systemName: "paperplane.fill") }
                    .disabled(draft.trimmingCharacters(in: .whitespaces).isEmpty)
            }.padding(.horizontal).padding(.bottom, 8)
        }.background(.bar)
    }
```

(`model.detailParticipants` = the agents of the open channel; add a tiny computed property mirroring `mentionableAgents` scoped to `detailChannelId`.)

- [ ] **Step 3: Make the HITL card actionable**

Replace v4b's display-only `hitlCard` with approve/deny buttons that call `model.respondHitl`. Parse `hitl_id` from the event payload (the `HitlRequest` carries `hitl_id`). Pass the `channelId` into `ChannelEventRow` (add it as a property) so the buttons know the channel:

```swift
    private var hitlCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Approval needed", systemImage: "exclamationmark.shield")
                .font(.subheadline.weight(.semibold)).foregroundStyle(.murOrange)
            if !event.text.isEmpty { Text(event.text).font(.footnote) }
            HStack {
                Button("Approve") { onRespond(true) }.buttonStyle(.borderedProminent).tint(.green)
                Button("Deny") { onRespond(false) }.buttonStyle(.bordered).tint(.red)
            }
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.murOrange.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
    }
```

where `onRespond` is an `(Bool) -> Void` closure the detail view supplies, calling `model.respondHitl(channelId: channelId, hitlId: <from payload>, allow:)`. The detail feed live-updates (the gate appends events + the channel.updated push refreshes), so the card resolves on approval.

- [ ] **Step 4: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh
git add mur-mobile-app/Sources/AppModel.swift mur-mobile-app/Sources/ChannelDetailView.swift
git commit -m "feat(ios): channel send + @mention autocomplete + actionable HITL card (v4c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Quality gates + E2E + docs

- [ ] **Step 1: Gates**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-core -p mur-daemon -p mur-mobile-sdk -- -D warnings
cargo nextest run -p mur-common -p mur-core -p mur-channel -p mur-mobile-sdk
cd mur-mobile-app && ./build-ios.sh && cd -
```

- [ ] **Step 2: E2E (needs daemon + phone/sim + v3c gate)**

```
1. Drop-into-channel: from the phone, open a CLI/Hub-born channel → send a message →
   it lands in THAT channel (not a new concierge one); Hub/CLI see it.
2. @mention: type "@" → autocomplete shows participants/agents; pick one → "@qa "; send.
   The text reaches the concierge channel; if v3b is live, a Delegation(Router) event appears.
3. Phone HITL respond: run a high-risk workflow over a channel (v3c) so it blocks on a
   HitlRequest → the phone home pins it (input-required) → open detail → Approve →
   the gate releases and the step runs; the channel returns to working→completed.
   Confirm the HitlResponse event is v3d-signed (sig present) and the gate accepted it.
```

- [ ] **Step 3: Docs + memory**

- `mur-mobile-app/README.md`: the phone can now send into any channel, @mention specialists (scoping hint), and approve/deny HITL gates.
- Memory: v4c done — channel_id-on-send + @mention + authoritative phone HITL respond (daemon writes v3d-signed HitlResponse on behalf of the paired phone).

- [ ] **Step 4: Commit**

```bash
git add mur-mobile-app/README.md
git commit -m "docs(ios): drop-into-channel, @mention, phone HITL respond (v4c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (`2026-06-16-unified-channel-v4-ios-design.md` §6 v4c + §4.2):**
- "read-and-respond into Hub/CLI-originated channels" → Task 1 (`channel_id`-on-send) + Task 4 (detail send affordance). ✓
- "@mention-as-scoping-hint … rendered as a Delegation event authored by Agent{id:mur} Router … never opens a second socket" → Task 3 (parse/autocomplete) + Task 4 (UI); the text goes to the **concierge channel**, the concierge (v3b) renders the Delegation — the phone never dials a specialist. ✓
- "authoritative phone HITL respond (needs v3d)" → Task 2 — the daemon verifies the paired frame, then writes a **v3d-signed** `HitlResponse` the v3c gate verifies. Closes v4b's display-only HITL card. ✓ (this completes the v4 spec §7 "respond" once signing lands).

**2. Placeholder scan:** No "TBD"/"add later". The Rust cores (`persist_mobile_exchange_into`, `respond_hitl`) are complete + unit-tested; the SDK `channel_id`/`hitl_respond` + daemon handlers are concrete wiring (a `grep` locates the envelope builder); the Swift uses real SwiftUI. The `onRespond` closure + `detailParticipants` are named, small additions described in place.

**3. Type consistency:**
- `persist_mobile_exchange_into(home, agent, channel_id: Option<&str>, user, agent_text)` (Task 1) — `persist_mobile_exchange` delegates with `None`; daemon passes the extracted `channel_id`.
- `respond_hitl(home, agent, channel_id, hitl_id, allow, reason)` (Task 2) echoes the `HitlRequest.action_hash` so the v3c gate's hash check passes; writes via v3d `append_as_writer`. `HitlResponse` payload keys (`hitl_id, action_hash, allow, reason, surface`) match v3c's struct.
- `ClientFrame::HitlRespond{channel_id, hitl_id, allow, reason}` (Task 2) ↔ SDK `Command::HitlRespond` ↔ `hitlRespond(channelId:hitlId:allow:reason:)` (Swift, Task 4).
- `sendText(text:channelId:)` (SDK, Task 1) ↔ `sendToChannel(_:channelId:)` (AppModel, Task 4).
- `@mention` helpers `mentionToken`/`mentionCandidates`/`applyMention` (Task 3) consumed by the detail bar (Task 4).

**4. Scope check:** v4c completes the phone as a full participant: drop-into-channel + @mention + authoritative HITL respond, across `mur-common`/`mur-core`/`mur-daemon`/`mur-mobile-sdk` + SwiftUI. Rust cores unit-tested; SDK/daemon/Swift wiring build+manual. The HITL authority is sound **only because v3d signing is in place** (flagged); the gate-resolver extension to trust the daemon-written paired-phone response is the v3d↔v4c seam. Research-gated per the spec. Focused. ✓

No gaps found.
