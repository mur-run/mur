# mur-mobile-sdk

Mobile core SDK for the MUR voice app — a thin, mobile-safe Rust core shared by
the iOS app (now) and a future Android app, exposed through **UniFFI** (Swift
binding today; Kotlin from the same crate later).

Design: `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

## What it owns

- **Network A2A client** over WebSocket. The desktop dial path (`mur-core`'s
  `a2a_dial`) speaks Unix-domain sockets, which a phone cannot reach — so this
  crate adds the network transport both ends need.
- **Ed25519 envelope signing**, reused verbatim from `mur-common`
  (`bridge::envelope` + `identity`), so the Mac verifies the phone against the
  agent's `trusted_peers` allowlist.
- **Typed event stream** surfaced to Swift/Kotlin via a callback interface
  (connection lifecycle + conversation in one stream).

## What it deliberately excludes

- whisper.cpp / Kokoro voice engines — the **Hybrid** pipeline keeps STT/LLM/TTS
  on the Mac; the phone streams audio and does on-device `SFSpeech` partials in
  the app layer (P3). No ONNX/espeak in the mobile binary.
- Any GUI / desktop dependencies.

## Public surface (UniFFI)

| Item | Purpose |
|------|---------|
| `MobileConfig` | home dir, default agent (`mur`), optional relay URL |
| `MobileClient::new` | load/create the phone identity keypair |
| `.set_listener` | register the foreign `MobileEventListener` |
| `.public_key()` | phone pubkey to encode into the pairing QR |
| `.connect_lan(host, port, pair_token)` | dial the Mac endpoint + pair |
| `.send_text(text)` | send a turn; reply arrives as an event |
| `.disconnect()` | tear down |
| `MobileEvent` | `Connecting`/`Connected`/`Disconnected`/`Transcript`/`Reply`/`Error` |

## Wire protocol

See `src/protocol.rs`. Text-frame JSON over WebSocket: a `Hello` pairing
handshake, then `SignedEnvelope`-wrapped A2A `JsonRpcRequest`s; the server
mirrors agent activity back as `mobile.*` events (matching the Hub `EventBus`
names so the desktop shows the same conversation).

## Status

P1 in progress — SDK + LAN transport build and compile. The Mac-side WebSocket
endpoint (hosted in `mur-daemon`) and QR pairing are the next P1 pieces; relay
(`wss://` via mur-server) and the voice pipeline follow in P3–P4.

## Generating bindings (later)

The crate builds `cdylib` + `staticlib`; Swift bindings are generated with
`uniffi-bindgen` and packaged as an XCFramework in the app's CI (P2). Not
required to build/test the Rust core.
