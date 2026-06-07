# MUR Voice Mobile App — Design Spec

> **Date**: 2026-06-05
> **Status**: Ready for review (Phase 0 design capture; decisions locked via Q&A 2026-06-05)
> **Scope**: A native iOS voice companion app that talks to a user's local MUR agents, with a reusable Rust core SDK (`mur-mobile-sdk`) so Android can follow. Adds the missing **network transport** (LAN + relay) on both phone and Mac, a **Hybrid** voice pipeline, an interactive **椋鳥 (starling)** mascot, and **live mirroring** of the conversation into MUR Hub. This doc is Phase 0 (design). Implementation is phased P1–P5 below.

## Overview

Today MUR agents are reachable only **on the Mac itself**: the A2A client (`mur-core/src/a2a_dial.rs`) dials a **Unix domain socket** (`~/.mur/agents/<name>/running.lock` → `/tmp/.mur-<uuid>.sock`) or falls back to **ephemeral stdio**. Neither is reachable from a phone. The agent `profile.yaml` schema reserves `transport.tcp` and a webhook URL, but **no client code wires them**, and there is **no LAN discovery or relay client** in the repo.

This app delivers a **voice-first** way to talk to those agents from a phone — hold the orange button to speak, triple-tap for hands-free — while the **椋鳥 mascot** reacts to mic amplitude and conversation state, and the **same conversation appears live in MUR Hub** on the desktop. The privacy stance is **local-first**: all AI (STT, agent LLM, TTS) runs on the user's Mac; the cloud relay is a **dumb encrypted tunnel** that never sees plaintext and never runs a model.

The hard, net-new engineering is the **network transport layer** — it must be built on *both* ends. The crypto (Ed25519 `SignedEnvelope`), the message model (JSON-RPC `Task`/`Message`), and the voice engines (whisper.cpp + Kokoro, shipped in D1) already exist and are reused.

## Locked Decisions

Captured via Q&A on 2026-06-05. These are the inputs this spec encodes.

| # | Decision | Choice |
|---|----------|--------|
| 1 | **App framework** | **Native Swift (iOS first)**. Chosen for lowest mic/audio latency — voice is the product. Cross-platform reuse comes from the Rust core, not a cross-platform UI toolkit. |
| 2 | **Min iOS** | **iOS 17+** (SFSpeech streaming, Rive runtime, modern SwiftUI). |
| 3 | **Shared core** | **`mur-mobile-sdk`** Rust crate exposed via **UniFFI** → Swift binding now, **Kotlin binding for Android later** from the same core. |
| 4 | **Transport** | **Both** — LAN-direct (primary) + **relay fallback** via `mur-server` when off-LAN. |
| 5 | **Voice pipeline** | **Hybrid** — on-device `SFSpeech` for instant partial transcript (perceived-latency hiding); Mac-side whisper.cpp for the authoritative transcript + Kokoro TTS for the reply. |
| 6 | **AI location / privacy** | **All AI stays on the Mac** (STT, agent LLM, TTS). Relay forwards only end-to-end-signed envelopes; it has **zero plaintext access** and runs **no model**. No cloud AI ever. |
| 7 | **Mascot runtime** | **Rive** state machine (interactive, input-driven). Lottie/video are rejected for the interactive mascot; AI is used in the **authoring** pipeline only. |
| 8 | **Pairing / trust** | **QR pairing** (Hub shows pubkey + LAN address + one-time token) → phone keypair stored in **iOS Keychain**, added to the agent's `trusted_peers` allowlist. |
| 9 | **This round** | **Phase 0 (this doc)**. Implementation P1–P5 below; each lands as its own PR. |

## Research Foundation

Four `deep-research` runs + two codebase explorations informed this design. **Honesty note on the research harness:** the adversarial verifier in two of the runs returned mostly `vote 0-0` ("insufficient verification votes"), which it labels "refuted." That is **not** "proven false" — the cited primary sources (Rive's official iOS repo, Apple's Private Cloud Compute docs, the EDPS on-device-AI brief, OpenAI's Realtime API announcement, Mozilla's UniFFI repo) are legitimate and their substance is industry-standard. Decisions below lean on those sources + first-hand codebase facts, and flag anything genuinely unconfirmed.

- **Voice UX (verified)**: streaming **speech-to-speech beats chained STT→LLM→TTS** on latency and prosody; a **VAD** model drives both endpointing ("user stopped speaking") and **barge-in** (interrupting TTS); an **earcon should mark the entry into the listening state**, but earcons are **weak signifiers** — pair them with visual + verbal cues, never rely on sound alone. (OpenAI Realtime API; NN/g audio-signifiers.) Specific latency thresholds and "PTT vs open-mic" trade-off numbers did **not** survive verification and are treated as conventional practice, not fact.
- **Rust mobile SDK (verified)**: **UniFFI** is the best-supported path for one Rust core → Swift **and** Kotlin (Mozilla-maintained, shipped in Firefox mobile), caveat **pre-1.0** (0.31.x) so advanced async/callback usage can break across upgrades — pin the version and add integration tests. **swift-bridge** supports async Rust→Swift but **Swift→Rust closures are not implemented** (bad for foreign-callback streaming) and is Swift-only — rejected for Android reuse. **CXX** lacks cross-FFI async and targets C++ — rejected. Real-world binding choices (Signal=JNI+cbindgen, matrix-rust-sdk=UniFFI) were **not** verified here; treat as indicative.
- **AI animation (sources consistent)**: for a stateful, touch- and amplitude-reactive UI mascot the right runtime is **Rive** — state machines with **Boolean/Trigger/Number inputs**, interruptible/blended transitions, **SwiftUI + UIKit** runtime via SPM, **runtime data-binding** to feed live values (mic RMS → a Number input), and **viseme-based lip-sync** (e.g. Rhubarb → `VisemeID` number) for the speaking state; a cited benchmark put Rive ~60fps vs Lottie ~17fps. AI **video** tools (Runway/Pika/Kling/Sora/Luma) produce **pre-rendered clips** — unsuitable for an interactive mascot; AI's role is **authoring** (concept art, auto-rig/inbetween) not runtime. (rive-app/rive-ios; callstack; dev.to.)
- **On-device privacy on M1 (sources consistent; harness unverified)**: fully-local STT (whisper.cpp, Metal-accelerated, tiny/base ~1GB, small ~2GB) + local small LLM (MLX, ~2–4GB at Q4) + local TTS (Kokoro 82M, shipped) is **feasible** on an M1 Air, but **8GB unified memory is tight** (shared with the OS) and the **fanless Air will thermally throttle under sustained voice load**. Industry default is **hybrid** (local instant-feedback + heavier lifting elsewhere); **fully-local is the right call for privacy-sensitive/local-first products**, which is MUR's positioning. → We keep **all AI on the Mac** and make model sizes **configurable** (no hardcoded model tier) so an 8GB Air can downshift.
- **`mur-server` relay (codebase fact)**: a relay **already exists** — Go + chi + `gorilla/websocket` on **Fly.io (Tokyo)**. `GET /api/v1/relay/ws` + an in-memory hub (`internal/relay/hub.go`) forward **command→result** between dashboard/API and a connected agent, with **JWT / API-key / device-code OAuth** auth and **device registration + heartbeat**. Gaps for our use: it is **request/response, not streaming**; the hub is **in-memory single-instance** (Fly auto-scale would split phone and Mac onto different machines → needs **single-machine pin or Redis pub/sub**); and there is **no mobile client** yet.

## Architecture

Three net-new components (iOS app, `mur-mobile-sdk`, Mac-side network endpoint), reusing existing crypto, message model, voice engines, and the Hub event bus.

```
┌──────────────────────────────────────────────────────────────────────┐
│  iOS App (Swift, iOS 17+)                                              │
│   • SwiftUI shell · Rive 椋鳥 state machine (idle/listening/thinking/  │
│       speaking/touch/error/launch) driven by mic-RMS + app state      │
│   • AVAudioEngine capture/playback · Core Haptics · earcons           │
│   • SFSpeech on-device → instant PARTIAL transcript (latency hiding)   │
│   • Orange button: hold = PTT · triple-tap = hands-free (open-mic)     │
└───────────────┬──────────────────────────────────────────────────────┘
                │  UniFFI (Swift bindings; callback-interface for streams)
┌───────────────▼──────────────────────────────────────────────────────┐
│  mur-mobile-sdk  (new Rust crate, workspace member)                    │
│   • Network A2A client over WebSocket (NOT unix socket)                │
│   • SignedEnvelope sign/verify  (reuse mur-common::bridge::envelope)   │
│   • JSON-RPC Task/Message model (reuse mur-common::a2a)                │
│   • Audio frame streaming (chunked) + event stream → Swift callbacks   │
│   • Transport selection: LAN ⇄ relay · reconnect/backoff · Keychain   │
│   ⚠️ does NOT bundle whisper/Kokoro — Hybrid keeps them on the Mac     │
└───────────────┬───────────────────────────────────┬──────────────────┘
   LAN (mDNS _mur._tcp, QR-paired)        relay (off-LAN, E2E-signed)
                │                                   │
                │                      ┌────────────▼─────────────┐
                │                      │ mur-server (Go, Fly.io)   │
                │                      │  /api/v1/relay/ws + hub   │
                │                      │  + stream_* frames (new)  │
                │                      │  + single-machine/Redis   │
                │                      │  forwards SignedEnvelope  │
                │                      │  bytes only — no plaintext│
                │                      └────────────┬─────────────┘
┌───────────────▼───────────────────────────────────▼──────────────────┐
│  Mac: MUR agent runtime + Hub (new network endpoint)                   │
│   • NEW: WS server endpoint phone can reach (LAN bind + relay client)  │
│   • Bonjour advertise _mur._tcp · QR pairing · trusted_peers check     │
│   • whisper.cpp authoritative transcript (mur-agent-runtime/voice/stt) │
│   • agent LLM (local, e.g. MLX) → A2A agent/send                       │
│   • Kokoro TTS streamed back (mur-agent-runtime/voice/tts)             │
│   • EventBus → Hub GUI renders the SAME conversation live              │
└───────────────────────────────────────────────────────────────────────┘
```

### Component responsibilities

**`mur-mobile-sdk` (new crate).** A thin, mobile-safe Rust core. It owns the network A2A client (WebSocket framing of JSON-RPC + audio chunks), envelope signing/verification, transport selection (LAN vs relay) with reconnect/backoff, and an event stream surfaced to Swift via a UniFFI **callback-interface**. It deliberately **excludes** the heavy voice deps (`whisper-rs`, `ort`, espeak-ng) and Tauri/hyper/whoami — those stay on the Mac. It depends on `mur-common` for `a2a`, `bridge::envelope`, `identity`, and `telemetry` constants.

**Mac-side network endpoint (new).** A WebSocket server the phone can reach. On LAN it binds a configurable port and advertises `_mur._tcp` via Bonjour; off-LAN it registers with the `mur-server` relay (reusing the device-code OAuth flow) and accepts forwarded frames. Every inbound frame is an Ed25519 `SignedEnvelope` checked against the agent's `trusted_peers`. It bridges phone↔agent traffic into the existing dial path and **publishes every turn onto the Hub `EventBus`** so the desktop mirrors the conversation. **Hosted in `mur-daemon`** (resolved 2026-06-05) so the phone reaches the agent even when the Hub GUI is closed. P1 targets the single default agent **`mur`** (concierge).

**iOS app (new).** Native Swift: SwiftUI shell, Rive mascot, AVAudioEngine capture/playback, SFSpeech partials, Core Haptics, and the orange-button interaction model. Talks only to `mur-mobile-sdk` via the generated Swift binding.

## Designed message flow (one "hold to speak" turn)

1. **Press** the orange button → haptic tap + a single "listening" earcon → mascot enters `Listening`; the button grows a live amplitude ring.
2. **Capture**: `AVAudioEngine` yields 16 kHz mono PCM. Two parallel consumers:
   - (a) **on-device `SFSpeech`** streams a **partial transcript** shown immediately (hides network + whisper latency);
   - (b) the SDK streams audio **frames** to the Mac over WebSocket (`stream_start` → `stream_chunk*`).
3. **End of utterance**: PTT release, or — in hands-free mode — **VAD endpointing** decides the user stopped. SDK sends `stream_end`; mascot → `Thinking`.
4. **Mac**: whisper.cpp produces the **authoritative** transcript (reconciles/overrides the on-device partial), which is fed to the agent via A2A `agent/send`; the agent reasons.
5. **Reply**: the agent streams reply text; Kokoro TTS streams audio back; mascot → `Speaking` with viseme lip-sync. **Barge-in**: if the user presses the button or (hands-free) speaks, VAD cancels TTS playback and returns to `Listening`.
6. **Hub mirror**: each turn (user transcript + agent reply, with state transitions) is published on `EventBus` as `mobile.transcript` / `mobile.reply` events; the Hub GUI subscribes and renders the **same conversation at the same time**.

All step-4/5 AI runs **on the Mac**. The phone only does capture, on-device partial ASR (UI only), and playback.

## Transport & security

- **LAN (primary)**: Mac advertises `_mur._tcp` via Bonjour; phone discovers and connects over WebSocket on the local network — lowest latency, no cloud.
- **Pairing**: Hub displays a **QR** encoding `{ agent_pubkey, lan_addr, one_time_token }`. The phone generates an Ed25519 keypair (stored in **iOS Keychain**), completes a token handshake, and the Mac adds the phone's pubkey to the agent's `trusted_peers`. One-time token prevents drive-by pairing.
- **Auth on every message**: frames are `SignedEnvelope { payload, sig, key_version, bridge_pubkey_multibase }` (reused verbatim from `mur-common/src/bridge/envelope.rs`); the Mac verifies signature + allowlist before dialing the agent.
- **Relay (fallback, off-LAN)**: connect to `mur-server` `/api/v1/relay/ws`; authenticate via the existing **device-code OAuth** flow. The relay forwards **signed envelope bytes only** — it never holds plaintext and runs no model (end-to-end confidentiality preserved even over the cloud). Server work required: add **streaming frame types** (`stream_start/chunk/end`) and either **pin to a single Fly machine** or add **Redis pub/sub** so phone and Mac land on the same hub.
- **No hardcoded endpoints/ports**: relay base URL, LAN port, Bonjour service type, and reconnect/backoff parameters are config-driven (`~/.mur/config.yaml` + app settings), per repo rule #1.

## Voice pipeline (Hybrid, all-local AI)

- **On-device (phone)**: `SFSpeech` streaming recognizer → partial transcript for instant UI feedback only. Never authoritative; never the privacy boundary for storage.
- **Mac (authoritative)**: whisper.cpp (`mur-agent-runtime/src/voice/stt.rs` — `WhisperTranscriber`, `VadGate`) for the final transcript; the agent's **local** LLM for reasoning; Kokoro (`mur-agent-runtime/src/voice/tts.rs` — `KokoroTts`, 24 kHz) for the spoken reply, streamed back.
- **Endpointing & barge-in**: reuse/extend `VadGate` (RMS energy gate today) for utterance endpointing in hands-free mode and to trigger barge-in cancellation of `VoicePlayer` (`mur-gui-core/src/voice/synth.rs` exposes `abort()`).
- **Model tiers configurable**: whisper model size and the agent LLM are config values so an 8GB M1 Air can downshift (e.g. whisper `base`, a 2–3B MLX model) to stay within memory and thermal budget. Document the fanless-Air sustained-load caveat in app/help text.
- **Optional fully-offline phone mode (P4+/deferred)**: a future toggle could run whisper on-device too (no audio leaves the phone), at the cost of app size and battery — gated behind the privacy research caveat; **not** in the first cut.

## 椋鳥 mascot & interaction design

**Runtime: Rive** (one `.riv` file with a state machine). Inputs:

| Input (Rive) | Type | Driven by |
|---|---|---|
| `state` | Number/enum | app state: idle / listening / thinking / speaking / error |
| `level` | Number | live mic RMS (data-binding, smoothed) → body bob / waveform |
| `viseme` | Number | TTS viseme stream → mouth shapes while speaking |
| `onTouch` | Trigger | user taps the bird → bounce + chirp |
| `isOffline` | Boolean | transport down → drooped/error pose |

**Mascot states**: `Idle` (breathing, blink, occasional head-tilt/scratch for charm) · `Listening` (head cocks, body bobs to amplitude, beak open) · `Thinking` (chin-tap/spin, "…") · `Speaking` (viseme lip-sync + sway) · `Touch` (bounce + chirp) · `Error/Offline` (droop, "?") · `Launch` (the **funny loading** animation — fly-in + tumble + feather-preen).

**Orange button** (your spec):

| Gesture | Behavior | Mode |
|---|---|---|
| **Hold** | Push-to-talk; release to send | Deliberate, single-shot (most private, least battery) |
| **Triple-tap** | Toggle **hands-free / always-speech** | Continuous conversation: VAD endpointing + barge-in |
| **Tap the bird** | Easter-egg: bounce, chirp, wing-flap | Any state |

**UI layout** (your wireframe):

```
┌─────────────────────┐
│                     │
│        🐦 椋鳥        │  ← Rive mascot, primary visual; reacts to touch + state
│                     │
│   ┌─────────────┐   │
│   │  partial …  │   │  ← live on-device transcript
│   └─────────────┘   │
│                     │
│      ●  speech      │  ← big orange button, one-handed reach; ring = mic level
└─────────────────────┘
```

**Multimodal feedback (research-driven)**: every state change is signaled **three ways** — mascot animation + button color-ring + haptic — because earcons are weak signifiers; the listening earcon fires only on entry to `Listening`. Accessibility: large single-hand target, VoiceOver labels per state, Reduce-Motion fallback that swaps mascot motion for a calm pulsing orb.

**Authoring pipeline (AI-assisted, not AI-runtime)**: concept art (AI image gen) → vector rig in the Rive editor → state machine wired to the inputs above → exported `.riv` loaded via SPM. Placeholder `.riv` first so engineering proceeds before final art (open question #2).

## Hub live-mirroring

The Mac endpoint publishes each turn on the existing `EventBus` (`mur-gui-core/src/event_bus.rs`, `HubEvent { agent_id, name, payload }`) under new namespaced events (`mobile.transcript`, `mobile.reply`, `mobile.state`). The Hub GUI already subscribes to the bus (it renders `voice.*`, `companion.message.new`, etc.), so mirroring is mostly a new renderer + event names — satisfying "show the messages in MUR Hub at the same time."

## Packaging & CI

- **UniFFI** binding; pin the version (pre-1.0) and add a binding smoke test. Streaming surfaced via **callback-interface** (Rust → Swift), avoiding the swift-bridge Swift→Rust-closure gap.
- **iOS**: build `aarch64-apple-ios` + simulator targets, assemble a **universal XCFramework**, integrate via **Swift Package Manager**; code-sign in the app's pipeline.
- **Android (future)**: `cargo-ndk` → `.so` for `arm64-v8a`/`x86_64` → AAR/Gradle, **same core crate** — the whole reason for UniFFI.
- **Binary size**: strip/LTO, exclude desktop-only deps from the mobile crate's feature set.

## Phased delivery

| Phase | Content | Exit criterion |
|---|---|---|
| **P0** | This spec | Approved |
| **P1** | `mur-mobile-sdk` crate + UniFFI scaffold + **LAN WebSocket transport** + **Mac WS endpoint** + Bonjour + QR pairing. **Text round-trip only** (no voice) | Phone sends text to a local agent over LAN and gets a reply; envelope-signed; appears in Hub |
| **P2** | iOS app shell: SwiftUI + **Rive mascot state machine** (placeholder art) + orange button (PTT + triple-tap) + text chat + **Hub mirror** | Interactive app, text conversation, mascot reacts to state, Hub shows same thread |
| **P3** | **Voice**: AVAudioEngine + SFSpeech partials + audio-frame streaming + Mac whisper final + Kokoro playback + **VAD endpointing + barge-in** | Full Hybrid voice turn works on LAN; barge-in cancels TTS |
| **P4** | **Relay fallback**: `mur-server` streaming frames + single-machine/Redis + device-code auth in app + reconnect + Keychain hardening | Phone talks to Mac agent while off-LAN, E2E-signed |
| **P5** | Polish: launch animation, final mascot art, haptics, earcons, accessibility (VoiceOver/Reduce-Motion), error recovery | Ship-quality UX |

## Open questions

Resolved 2026-06-05 (1–3); remaining deferred.

1. **Mascot art** — ✅ **Use a `.riv` file** (Rive). Engineering proceeds on a placeholder `.riv`; final art swaps in at P5.
2. **Mac endpoint host** — ✅ **`mur-daemon`** (always-on, so the phone reaches the agent even when the Hub GUI is closed).
3. **P1 target agent** — ✅ **single default agent `mur`** (the concierge; `display_name` MUR). Multi-agent picker (`AgentDiscovery`/`AgentEntry`) deferred to P2.
4. **Relay scaling** *(deferred, P4)* — pin to a single Fly machine (prototype) vs add Redis pub/sub (production).
5. **Fully-offline phone STT** *(deferred, past first cut)* — ship the on-device-whisper privacy toggle, or rely on the Mac (Hybrid) only.

## Risks

- **Net-new transport on both ends** (mDNS/pairing/relay-streaming) is the largest unknown and the critical path; P1 de-risks it with a text-only round-trip before voice.
- **UniFFI pre-1.0** async/callback churn — pin + integration tests.
- **iOS background audio / AVAudioSession** and App Store review for continuous mic use.
- **Hybrid transcript reconciliation** — on-device partial vs Mac whisper final needs a clear override/correction UX so the user isn't confused by changing text.
- **8GB M1 Air** — sustained local STT+LLM+TTS may throttle; configurable model tiers + honest help-text mitigate.

## Reused existing code (no reinvention)

| Need | Reuse | Path |
|---|---|---|
| Crypto envelopes | `SignedEnvelope`, `sign_payload`, `verify_envelope_with_pubkey` | `mur-common/src/bridge/envelope.rs` |
| Identity keypair | `AgentIdentity`, `encode/decode_pubkey` | `mur-common/src/identity.rs` |
| Message model | `JsonRpcRequest/Response`, `Task`, `Message`, `MessagePart` | `mur-common/src/a2a.rs` |
| Dial path (Mac side) | `dial_method`, `canonicalize_agent_name`, `DialMode` | `mur-core/src/a2a_dial.rs` |
| STT | `WhisperTranscriber`, `VadGate` | `mur-agent-runtime/src/voice/stt.rs` |
| TTS | `KokoroTts`, `KokoroTokenizer` | `mur-agent-runtime/src/voice/tts.rs` |
| TTS playback + abort (barge-in) | `VoicePlayer::speak/abort` | `mur-gui-core/src/voice/synth.rs` |
| Hub mirroring | `EventBus`, `HubEvent` | `mur-gui-core/src/event_bus.rs` |
| Agent listing | `AgentDiscovery`, `AgentEntry` | `mur-gui-core/src/discovery.rs` |
| Relay (extend) | `/api/v1/relay/ws`, hub, device-code OAuth | `mur-server/internal/relay/hub.go`, `…/handlers/relay_handler.go`, `…/handlers/device_auth.go` |
