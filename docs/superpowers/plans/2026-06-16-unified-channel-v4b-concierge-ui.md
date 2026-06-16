# Unified Channel v4b — Concierge-First iOS UI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> Implements the concierge-first UI from `2026-06-16-unified-channel-v4-ios-design.md` §4.1 + §6 v4b. **Depends on v4a** (the SDK channel types + read RPCs + live push) — v4a is **planned, not yet built**; this plan consumes its `MobileEvent::{ChannelList, ChannelEvents, ChannelUpdate}` + `ChannelListItem`/`ChannelEventItem` + `MobileClient::{list_channels, fetch_channel_events}`. Adjust the type names if v4a's land differently.

**Goal:** turn the single-screen voice app into a concierge-first home — a top talk-zone (the existing hold-to-talk MUR affordance) plus a glanceable, `InputRequired`-first Channel list — that drills into a read-only channel-detail event feed with per-`EventKind` rendering, live-refreshed while connected. The phone becomes a window onto the user's durable Channels (born on phone, Hub, or CLI), with the HITL gate surfaced for at-a-glance triage.

**Architecture:** pure SwiftUI over the v4a SDK. `AppModel` (`@Observable`) gains a `channels` list + selected-channel events + handlers for the three new `MobileEvent`s; the existing voice/transcript path (the concierge thread) is untouched. All branching/formatting logic (sort, state→chip, actor→label, event→row variant, relative time) is extracted into **pure free functions** in a new `ChannelFormatting.swift` so the views stay declarative. Navigation becomes a two-level `NavigationStack` (home → channel-detail). **v4b is read + triage:** the home talk-zone sends to the concierge channel via the existing `send_text` (v4a resolves it by agent); channel-detail is a **read-only** feed (sending *into an arbitrary* channel needs `channel_id`-on-send and is **v4c**). A `HitlRequest` event renders as a prominent card that **displays** the pending approval and routes the user to respond on desktop — the phone-side respond/write needs v3c (the gate) + a write RPC + ideally v3d signing, so it is **deferred** (see §scope).

**Tech Stack:** SwiftUI (`@Observable` `AppModel`, `NavigationStack`), built via XcodeGen (`project.yml` → `MurVoice.xcodeproj`) + `build-ios.sh`. Consumes the regenerated `mur_mobile_sdk.swift` bindings from v4a. **No Rust changes.**

**Scope guardrails (from the v4 spec §4.1, §6 v4b, §7):**
- The unit is the **Channel (goal)**, never an agent roster. No "add agent", no per-agent config.
- Home talk-zone addresses the **concierge channel only** (existing `send_text`). **No send-into-arbitrary-channel** in v4b (that needs `channel_id`-on-send → v4c).
- Channel-detail is **read-only + live-refresh** in v4b.
- `HitlRequest` is **displayed** (triage), with a "respond on desktop" affordance; phone-side respond is **deferred** (needs v3c + a write RPC + v3d signing for high-risk authority). Do **not** ship a card that looks like it authoritatively approves a high-risk gate.
- **Do not over-promise notifications**: live refresh fires only while connected (APNs is v4d). Show a "what's new since you were away" via catch-up fetch on connect.

**Key facts locked during exploration (do not re-derive):**
- `AppModel` (`AppModel.swift`): `@MainActor @Observable`; `ChatLine{role,text}`; `transcript: [ChatLine]`; `connectedAgent: String?`; `isConnected`; `handle(_ event: MobileEvent)` switch (`:364`); `send`/`sendTyped` (`:356`,`:175`); voice via `OrangeButton`/`StarlingMascot`. `EventBridge` bridges SDK events to `handle` on the main actor.
- `ContentView` (`ContentView.swift`): **no NavigationStack today**; two layout branches keyed on `model.transcript.isEmpty`; `header`, `transcriptView`/`bubble`, `typeBar`, `statusLine`; sheets for pairing/settings.
- Build: XcodeGen auto-globs `Sources/` — new `.swift` files under `Sources/` are picked up by `xcodegen generate` (run by `build-ios.sh`). **No XCTest target exists** → v4b is verified by build + manual; pure helpers are free functions (unit-testable once a test target is added).
- `send_text(text)` carries **no** `channel_id` (v4a resolves the channel by agent) — hence channel-detail is read-only in v4b.
- Theme colors: `Color.murBlue`/`Color.murOrange` (`Theme.swift`); `StarlingMascot(state:micLevel:)`, `OrangeButton(...)`, `MascotState`.

---

## File Structure

**Created:**
- `mur-mobile-app/Sources/ChannelFormatting.swift` — pure view-model helpers + the `ChannelEventRow` variant enum.
- `mur-mobile-app/Sources/ChannelListView.swift` — the home's Channel-list section + `ChannelCard`.
- `mur-mobile-app/Sources/ChannelDetailView.swift` — the read-only event feed + per-`EventKind` row views + the HITL display card.

**Modified:**
- `mur-mobile-app/Sources/AppModel.swift` — `channels`/`detailEvents` state, new `MobileEvent` handling, `refreshChannels`/`openChannel`.
- `mur-mobile-app/Sources/ContentView.swift` — wrap in `NavigationStack`; add the Channel-list section below the talk-zone; nav to `ChannelDetailView`.

No Rust changes.

---

## Task 1: AppModel — channel state + event handling

**Files:**
- Modify: `mur-mobile-app/Sources/AppModel.swift`

- [ ] **Step 1: Add the Swift-side channel value types + state**

In `AppModel.swift`, add value types mirroring the v4a SDK records (the SDK delivers `ChannelListItem`/`ChannelEventItem`; we keep app-local `Identifiable` copies for SwiftUI):

```swift
    struct ChannelSummary: Identifiable, Equatable {
        let id: String
        let title: String
        let state: String       // kebab ChannelState
        let goal: String
        let updatedAt: String
        let agents: [String]
        let turns: Int
    }
    struct ChannelEventVM: Identifiable, Equatable {
        var id: UInt64 { seq }
        let seq: UInt64
        let ts: String
        let actorKind: String   // "human" | "agent" | "system"
        let actorName: String
        let kind: String        // "message" | "state-change" | …
        let text: String
    }
```

Add observable state (near `transcript`, `:17`):

```swift
    private(set) var channels: [ChannelSummary] = []
    private(set) var detailChannelId: String?
    private(set) var detailEvents: [ChannelEventVM] = []
```

- [ ] **Step 2: Handle the three new `MobileEvent`s**

In `handle(_ event: MobileEvent)` (`:364`), add cases (v4a defines these variants):

```swift
        case let .channelList(items):
            channels = items.map {
                ChannelSummary(id: $0.id, title: $0.title, state: $0.state, goal: $0.goal,
                               updatedAt: $0.updatedAt, agents: $0.agents, turns: Int($0.turns))
            }
        case let .channelEvents(channelId, events):
            guard channelId == detailChannelId else { break }
            detailEvents = events.map {
                ChannelEventVM(seq: $0.seq, ts: $0.ts, actorKind: $0.actorKind,
                               actorName: $0.actorName, kind: $0.kind, text: $0.text)
            }
        case let .channelUpdate(channelId):
            // A channel changed while connected — refresh the list, and the open
            // detail if it's the one that changed.
            client?.listChannels()
            if channelId == detailChannelId { client?.fetchChannelEvents(channelId: channelId, sinceSeq: nil) }
```

> If the generated Swift enum cases are named differently (e.g. `.channel_list`), match the bindings. UniFFI lower-camel-cases by default → `.channelList`.

- [ ] **Step 3: Add refresh/open commands**

Add methods (near `sendTyped`, `:175`):

```swift
    /// Pull the owner's channel list (result arrives as `.channelList`).
    func refreshChannels() { client?.listChannels() }

    /// Open a channel's detail feed: select it and fetch its events.
    func openChannel(_ id: String) {
        detailChannelId = id
        detailEvents = []
        client?.fetchChannelEvents(channelId: id, sinceSeq: nil)
    }
    func closeChannel() { detailChannelId = nil; detailEvents = [] }
```

Also call `refreshChannels()` once on connect — in `handle`'s `.connected` case (`:368`), after `mascot = .idle`, add `client?.listChannels()`.

- [ ] **Step 4: Build**

Run: `cd mur-mobile-app && ./build-ios.sh` (or `xcodebuild -project MurVoice.xcodeproj -scheme MurVoice -destination 'generic/platform=iOS' build`). Expected: compiles against the v4a bindings. (If v4a's SDK isn't built yet, this is the integration point — build after v4a lands.)

- [ ] **Step 5: Commit**

```bash
git add mur-mobile-app/Sources/AppModel.swift
git commit -m "feat(ios): AppModel channel state + ChannelList/Events/Update handling (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: Pure formatting helpers

**Files:**
- Create: `mur-mobile-app/Sources/ChannelFormatting.swift`

> SwiftUI views aren't unit-tested (no XCTest target). These are pure free functions so the views stay declarative and the logic is reviewable/testable-if-a-target-is-added. Verified by build + the manual checks in Task 6.

- [ ] **Step 1: Write the helpers**

```swift
import SwiftUI

/// How a channel event renders in the detail feed.
enum EventRowVariant: Equatable {
    case userMessage      // human, right-aligned
    case agentMessage     // agent, left-aligned
    case note             // system note / separator
    case state            // state-change chip
    case delegation       // thin "A → B" separator
    case tool             // collapsed tool call/result one-liner
    case artifact         // openable card
    case hitl             // prominent approval card (display)
    case other            // forward-compatible fallback card
}

/// Channels sorted for the home list: input-required first (needs the human),
/// then most-recently-updated. Pure; `updatedAt` is RFC3339 so string-desc works.
func sortedChannels(_ channels: [AppModel.ChannelSummary]) -> [AppModel.ChannelSummary] {
    channels.sorted { a, b in
        let aBlocked = a.state == "input-required"
        let bBlocked = b.state == "input-required"
        if aBlocked != bBlocked { return aBlocked }   // blocked first
        return a.updatedAt > b.updatedAt              // newest first
    }
}

/// State → (label, color) for the lifecycle chip.
func stateChip(_ state: String) -> (label: String, color: Color) {
    switch state {
    case "working":        return ("working", .murBlue)
    case "input-required": return ("needs you", .murOrange)
    case "completed":      return ("done", .green)
    case "failed", "rejected": return (state, .red)
    case "submitted", "stale", "canceled": return (state, .gray)
    default:               return (state, .gray)
    }
}

/// Decide the render variant for an event.
func eventVariant(actorKind: String, kind: String) -> EventRowVariant {
    switch kind {
    case "message":
        switch actorKind {
        case "human": return .userMessage
        case "agent": return .agentMessage
        default:      return .note
        }
    case "note":          return .note
    case "state-change":  return .state
    case "delegation", "handoff": return .delegation
    case "tool-call", "tool-result": return .tool
    case "artifact":      return .artifact
    case "hitl-request":  return .hitl
    default:              return .other
    }
}

/// "tool-call" → "Tool Call" for fallback card headers.
func eventKindLabel(_ kind: String) -> String {
    kind.split(separator: "-").map { $0.prefix(1).uppercased() + $0.dropFirst() }.joined(separator: " ")
}

/// A short author label for the feed.
func actorLabel(actorKind: String, actorName: String) -> String {
    switch actorKind {
    case "human":  return actorName.isEmpty ? "You" : actorName
    case "agent":  return actorName.isEmpty ? "agent" : actorName
    default:       return "system"
    }
}
```

- [ ] **Step 2: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh
git add mur-mobile-app/Sources/ChannelFormatting.swift
git commit -m "feat(ios): pure channel formatting helpers (sort/chip/variant) (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: Channel-list section + card

**Files:**
- Create: `mur-mobile-app/Sources/ChannelListView.swift`

- [ ] **Step 1: Write the list + card**

```swift
import SwiftUI

/// The home's Channel list (below the talk zone). Cards are NavigationLinks into
/// the channel-detail feed. Sorted input-required-first via `sortedChannels`.
struct ChannelListView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        LazyVStack(spacing: 8) {
            ForEach(sortedChannels(model.channels)) { ch in
                NavigationLink(value: ch.id) {
                    ChannelCard(channel: ch)
                }
                .buttonStyle(.plain)
            }
            if model.channels.isEmpty {
                Text("No channels yet — talk to MUR above to start.")
                    .font(.footnote).foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity).padding(.vertical, 24)
            }
        }
    }
}

struct ChannelCard: View {
    let channel: AppModel.ChannelSummary

    var body: some View {
        let chip = stateChip(channel.state)
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(channel.goal.isEmpty ? channel.title : channel.goal)
                    .font(.subheadline.weight(.semibold)).lineLimit(1)
                Spacer()
                Text(chip.label)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 8).padding(.vertical, 2)
                    .background(chip.color.opacity(0.18), in: Capsule())
                    .foregroundStyle(chip.color)
            }
            HStack {
                Text(channel.agents.joined(separator: ", "))
                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                Text("\(channel.turns) turns").font(.caption2).foregroundStyle(.tertiary)
            }
        }
        .padding(12)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(channel.goal). \(chip.label). agents \(channel.agents.joined(separator: ", "))")
    }
}
```

- [ ] **Step 2: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh
git add mur-mobile-app/Sources/ChannelListView.swift
git commit -m "feat(ios): Channel-list section + card (input-required-first) (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: Channel-detail event feed

**Files:**
- Create: `mur-mobile-app/Sources/ChannelDetailView.swift`

- [ ] **Step 1: Write the detail view + per-variant rows**

```swift
import SwiftUI

/// Read-only event feed for one channel (the phone projection of the Hub Work
/// view's three panes, collapsed to one timeline). Live-refreshed while
/// connected. Sending INTO an arbitrary channel is v4c — this view is read +
/// HITL-display only.
struct ChannelDetailView: View {
    @Environment(AppModel.self) private var model
    let channelId: String

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(model.detailEvents) { ev in
                        ChannelEventRow(event: ev).id(ev.id)
                    }
                }
                .padding()
                .onChange(of: model.detailEvents.count) { _, _ in
                    if let last = model.detailEvents.last {
                        withAnimation { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }
        }
        .navigationTitle("Channel")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { model.openChannel(channelId) }
        .onDisappear { model.closeChannel() }
    }
}

struct ChannelEventRow: View {
    let event: AppModel.ChannelEventVM
    @State private var expanded = false

    var body: some View {
        switch eventVariant(actorKind: event.actorKind, kind: event.kind) {
        case .userMessage:
            bubble(text: event.text, color: .murBlue, alignment: .trailing)
        case .agentMessage:
            VStack(alignment: .leading, spacing: 2) {
                Text(actorLabel(actorKind: event.actorKind, actorName: event.actorName))
                    .font(.caption2).foregroundStyle(.secondary)
                bubble(text: event.text, color: .murOrange, alignment: .leading)
            }
        case .note:
            Text(event.text).font(.footnote).italic().foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
        case .state, .delegation:
            Text(separatorText).font(.caption2).foregroundStyle(.tertiary)
                .frame(maxWidth: .infinity, alignment: .center)
        case .tool:
            DisclosureGroup(isExpanded: $expanded) {
                Text(event.text).font(.caption.monospaced()).foregroundStyle(.secondary)
            } label: {
                Label(eventKindLabel(event.kind), systemImage: "wrench.and.screwdriver")
                    .font(.caption)
            }
        case .artifact:
            card(title: "Artifact", body: event.text, accent: .murBlue)
        case .hitl:
            hitlCard
        case .other:
            card(title: eventKindLabel(event.kind), body: event.text, accent: .gray)
        }
    }

    private var separatorText: String {
        event.text.isEmpty ? eventKindLabel(event.kind) : event.text
    }

    private func bubble(text: String, color: Color, alignment: Alignment) -> some View {
        Text(text)
            .padding(10)
            .background(color.opacity(0.15), in: RoundedRectangle(cornerRadius: 12))
            .frame(maxWidth: .infinity, alignment: alignment)
    }

    private func card(title: String, body: String, accent: Color) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(accent)
            if !body.isEmpty { Text(body).font(.footnote) }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(accent.opacity(0.4)))
    }

    // HITL: DISPLAY only in v4b. Authoritative approve/deny needs v3c (the gate)
    // + a mobile write RPC + v3d signing for high-risk authority — deferred.
    private var hitlCard: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label("Approval needed", systemImage: "exclamationmark.shield")
                .font(.subheadline.weight(.semibold)).foregroundStyle(.murOrange)
            if !event.text.isEmpty { Text(event.text).font(.footnote) }
            Text("Respond from the MUR desktop app.")
                .font(.caption2).foregroundStyle(.secondary)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.murOrange.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
    }
}
```

- [ ] **Step 2: Build + commit**

```bash
cd mur-mobile-app && ./build-ios.sh
git add mur-mobile-app/Sources/ChannelDetailView.swift
git commit -m "feat(ios): read-only channel-detail event feed + per-kind rows (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: Home — NavigationStack + two zones

**Files:**
- Modify: `mur-mobile-app/Sources/ContentView.swift`

- [ ] **Step 1: Wrap in a NavigationStack with the Channel list below the talk zone**

Restructure `ContentView.body` to a `NavigationStack` whose root is the two-zone home: the existing talk affordance on top (mascot + `OrangeButton` + `typeBar`, addressing the concierge channel as today), and the `ChannelListView` below it; a `navigationDestination` opens `ChannelDetailView`. Keep the `header`, sheets, and `onAppear { model.start() }`.

```swift
    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) {
                    // Talk zone (concierge): mascot + status. PTT/typeBar are pinned
                    // bottom via safeAreaInset (unchanged behavior).
                    VStack(spacing: 12) {
                        StarlingMascot(state: model.mascot, micLevel: model.micLevel)
                            .scaleEffect(model.transcript.isEmpty ? 1.0 : 0.6)
                        statusLine
                    }
                    .padding(.top, 8)

                    // Recent concierge turns (compact) so the talk zone shows context.
                    if !model.transcript.isEmpty { transcriptView }

                    Divider().padding(.horizontal)

                    // Channel list (the home's second zone).
                    HStack {
                        Text("Channels").font(.headline)
                        Spacer()
                        Button { model.refreshChannels() } label: { Image(systemName: "arrow.clockwise") }
                            .disabled(!model.isConnected)
                    }
                    .padding(.horizontal)
                    ChannelListView()
                        .padding(.horizontal)
                }
                .padding(.vertical, 8)
            }
            .navigationDestination(for: String.self) { channelId in
                ChannelDetailView(channelId: channelId)
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                header.padding(.horizontal).padding(.vertical, 8).background(.bar)
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 0) {
                    OrangeButton(
                        state: model.mascot, micMode: model.micMode,
                        onPressStart: { model.beginCapture() },
                        onPressEnd: { model.endCaptureAndSend() },
                        onTripleTap: { model.toggleMicMode() }
                    ).padding(.top, 8)
                    typeBar.padding(.horizontal).padding(.vertical, 8)
                }
                .background(.bar)
            }
            .scrollDismissesKeyboard(.interactively)
        }
        .sheet(isPresented: $showPairing) {
            PairingSheet { info in model.connect(host: info.host, port: info.port, token: info.token) }
        }
        .sheet(isPresented: $showSettings) { SettingsSheet() }
        .onAppear { model.start() }
    }
```

Keep `header`, `transcriptView`, `bubble`, `statusLine`, `typeBar` as-is. Delete the old `mainContent` empty-vs-conversation branch (its talk-zone parts are folded above; its logic is no longer needed). The bottom PTT/typeBar still drives the concierge thread via the existing `beginCapture`/`endCaptureAndSend`/`sendTyped`.

- [ ] **Step 2: Build (full app)**

Run: `cd mur-mobile-app && ./build-ios.sh`. Expected: the app builds; home shows the talk zone + a Channels section; tapping a channel pushes the detail feed.

- [ ] **Step 3: Commit**

```bash
git add mur-mobile-app/Sources/ContentView.swift
git commit -m "feat(ios): concierge-first home — NavigationStack + Channel list zone (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 6: Manual E2E + docs

> No XCTest target exists, so there are no Swift unit tests to run; v4b is verified by build + manual on a device/simulator against a real daemon. (If a test target is later added, the `ChannelFormatting.swift` free functions are the unit-test surface: `sortedChannels`, `stateChip`, `eventVariant`, `actorLabel`, `eventKindLabel`.)

- [ ] **Step 1: Build the app**

```bash
cd mur-mobile-app && ./build-ios.sh
```
Expected: clean build against the v4a bindings.

- [ ] **Step 2: Manual E2E (needs v4a daemon + a paired phone/simulator)**

```
1. With the daemon running + v4a built, create channels from other surfaces:
   - `mur agent cli mur` → send a couple of messages (CLI channel)
   - in the Hub, chat with an agent (Hub channel)
2. Pair the phone (QR / Bonjour / debug auto-connect UserDefaults).
3. HOME: the Channels section lists those channels, newest first; any
   input-required channel is pinned to the top with a "needs you" chip.
4. Talk zone: hold-to-talk / type → the concierge replies (voice + text), and
   the turn appears in the concierge channel (visible in Hub/CLI too).
5. Tap a channel card → channel-detail shows its event feed with per-kind rows
   (user/agent bubbles, notes, state separators; tool/artifact/hitl as cards).
6. Touch that channel from the CLI/Hub → within ~1s the detail feed live-updates
   (via the v4a channel.updated push); the home list reorders.
7. A HitlRequest event renders as an "Approval needed — respond on desktop" card
   (display only in v4b).
```

- [ ] **Step 3: Docs**

- `mur-mobile-app/README.md`: note the concierge-first home (talk zone + Channel list) + read-only channel detail; HITL is display-only on phone in v4b.
- No Rust/CLAUDE.md change (v4b is UI-only).

- [ ] **Step 4: Commit**

```bash
git add mur-mobile-app/README.md
git commit -m "docs(ios): concierge-first home + channel detail (v4b)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (against `2026-06-16-unified-channel-v4-ios-design.md` §4.1 + §6 v4b):**
- "two-zone home (talk zone + Channel-list cards + lifecycle chips + participant avatars + InputRequired sort)" → Task 5 (home) + Task 3 (cards) + Task 2 (`stateChip`, `sortedChannels`). ✓
- "channel-detail event feed with per-EventKind renderers" → Task 4 (`ChannelEventRow` over `eventVariant`). ✓
- "live-refresh while connected" → Task 1 (`.channelUpdate` handling) + Task 4 (`openChannel` on appear). ✓
- "HITL action card" → Task 4 `hitlCard` — **scoped to display + "respond on desktop"** per §7 (phone respond needs v3c + write RPC + v3d signing). Flagged. ✓
- "two send targets (concierge vs open channel)" → **refined:** v4b ships the concierge send target only; sending into an arbitrary channel needs `channel_id`-on-send (`send_text` has none today) and is **v4c**. v4b channel-detail is read-only. This resolves the §4.1-vs-§6-v4c tension decisively. ✓ (flagged in scope + architecture)
- "no agent roster / per-agent config" → the list unit is `ChannelSummary` (a goal), never an agent. ✓
- "don't over-promise notifications" → live refresh only while connected; catch-up via fetch-on-open; APNs is v4d. ✓

**2. Placeholder scan:** No "TBD"/"add styling later"/"similar to". Every view + helper is complete Swift. The HITL respond-write and arbitrary-channel send are **explicit deferrals** (to v4c / post-v3c), not placeholders. Two steps reference matching the generated binding case names (UniFFI camelCasing) — a real integration caveat, not a gap.

**3. Type consistency:**
- `AppModel.ChannelSummary{id,title,state,goal,updatedAt,agents,turns}` + `ChannelEventVM{seq,ts,actorKind,actorName,kind,text}` (Task 1) consumed by `sortedChannels`/`ChannelCard` (Tasks 2-3) and `ChannelEventRow` (Task 4) with matching field names.
- The SDK→app mapping in Task 1 (`.channelList`/`.channelEvents`/`.channelUpdate` → the structs) matches the v4a record fields (`ChannelListItem{id,title,state,goal,updated_at,agents,turns}`, `ChannelEventItem{seq,ts,actor_kind,actor_name,kind,text}`) — UniFFI lower-camel-cases, so `updated_at`→`updatedAt`, `actor_kind`→`actorKind`.
- `eventVariant(actorKind:kind:) -> EventRowVariant` (Task 2) drives `ChannelEventRow`'s switch (Task 4) — variants match 1:1.
- `model.refreshChannels()`/`openChannel(_)`/`closeChannel()` (Task 1) called from `ChannelListView` nav + `ChannelDetailView` lifecycle + `ContentView` refresh button.
- `NavigationLink(value: ch.id)` (String) ↔ `navigationDestination(for: String.self)` (Task 3 ↔ Task 5) — types match.

**4. Scope check:** Single sub-project (v4b), SwiftUI-only, 6 tasks, builds on v4a's read sync alone (no v3c/v3d dependency for the core; HITL respond + arbitrary-channel send correctly deferred to v4c / post-v3c). Verified by build + manual (no XCTest target — honest); pure logic extracted for review/future tests. Focused. ✓

No gaps found.
