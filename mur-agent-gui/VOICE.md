# mur-agent-gui — Voice (D1 / M1)

> **Frozen surface.** The default-off opt-in contract, the 12 voice
> commands, and the `voice_state.json` / `hotkey.json` storage layout
> are stable. Behavior changes go through a new minor-version
> milestone; format breakage requires a migration plan.

## Default-off contract

Fresh install starts disabled:
- No `PttButton` rendered.
- No mic capture.
- No STT model on disk.
- No global hotkey registered.
- `voice_state.json` does not exist.

The user opts in via **Settings → Voice → Enable voice**. That:

1. Triggers `voice_stt_download` (idempotent — re-uses existing
   model on disk if present).
2. Loads the default voice (`af_heart` if bundled / installed).
3. Registers the persisted PTT hotkey (or `Cmd+Shift+'` /
   `Ctrl+Shift+'` default).
4. Persists `enabled: true` to `<app_data>/voices/voice_state.json`.

Disable reverses all four steps without touching disk-installed models
(re-enable is fast, no re-download). Boot-time setup re-registers the
hotkey if `voice_state.json` says enabled (mirrors macOS TCC: opt-in
must outlive process restart, just like revocation).

## Storage layout

```
<app_data>/voices/
  voice_state.json                    # { enabled, updated_at }
  hotkey.json                         # { modifiers[], code }
  registry.json                       # installed-voice index
  af_heart/                           # bundled default voice
    voice.onnx
    manifest.json
    manifest.json.sig
  _stt/whisper-large-v3-turbo-q5_1/   # downloaded STT (~809 MB)
    ggml-large-v3-turbo-q5_1.bin
    manifest.json
    manifest.json.sig
```

All persisted writes are atomic temp+rename. Corrupt state files fall
back to safe defaults rather than blocking app startup.

## Tauri command surface (12)

| Command | Purpose |
|---|---|
| `voice_status` | `{enabled, default_voice_id, voices_installed, stt_installed, stt_loaded}` |
| `voice_enable` | full opt-in orchestration |
| `voice_disable` | reverses opt-in, frees RAM, persists |
| `voice_list_installed` | `{voices[], default_voice_id}` |
| `voice_set_default` | switch active voice |
| `voice_download` | per-voice CDN download; emits `voice://download-progress` |
| `voice_stt_status` | `{model_id, installed, loaded, size_bytes}` |
| `voice_stt_download` | STT model fetch; emits `voice://stt-download-progress` |
| `tts_speak` | synthesize default voice + play |
| `stt_transcribe_pcm16k` | wraps `SttEngine::transcribe` |
| `voice_start_capture` | begin mic capture (dedicated cpal thread) |
| `voice_stop_capture` | end + drain captured samples |
| `voice_get_hotkey` | current persisted PTT hotkey config |
| `voice_rebind_hotkey` | unregister old + register new + persist |

## Acceptance criteria (per roadmap §4.1)

| | Status |
|---|---|
| TTS = Kokoro 82M int8 ONNX (`ort` v2 rc.10) | ✅ session loader + streaming synthesis loop |
| STT = whisper.cpp `large-v3-turbo` q5_1 | ✅ `whisper-rs` 0.14 adapter; Metal feature gated to macOS |
| Hotkey rebindable (default `Cmd+Shift+'` / `Ctrl+Shift+'`; **not** Fn) | ✅ HotkeyRebinder UI + `voice_rebind_hotkey` |
| Bundle: 1 default voice (`af_heart`) installed; STT downloaded on first opt-in | ✅ `download_stt_model` + `download_voice` + bundled-voice seeding |
| Default-off (privacy + bandwidth + onboarding flow) | ✅ `voice_state.json` + `VoiceEnablePanel` + `PttButton` renders null when disabled |
| Signed CDN with SHA-256 + Ed25519 verify | ✅ `verify_and_parse` / `verify_signature` / streaming SHA-256 in `download_one_asset` |
| Atomic on-disk writes | ✅ temp+rename throughout |
| 250 ms PTT debounce | ✅ `PttFsm` Rust + `PttButton` TS both enforce |
| TTS first-byte ≤ 250 ms (M1; Apple Silicon) | ⏳ deferred to v1.0 release bench (needs ONNX fixture not in CI) |
| STT RTF ≤ 0.5× (M2 Apple Silicon) | ⏳ deferred to v1.0 release bench (needs GGUF fixture not in CI) |
| Voice cloning (user-uploaded reference audio) | ❌ deferred to v2 with AudioSeal watermark |
| Streaming / interruptible voice (Silero VAD) | ❌ deferred to v2 |

The two ⏳ rows are functional acceptance gates that require model
fixtures not currently shipped in CI (~900 MB total). The bench
harness skeleton is documented here; the actual measurements run on
the release engineer's macOS laptop before each `v1.0` tag (and on
the Apple Silicon CI runner once we add it).

## Test coverage

- **60 voice tests** pass across `voice/manifest`, `voice/registry`,
  `voice/audio/{ring_buffer, capture_worker, ptt}`, `voice/tts/{g2p,
  sentence_split}`, `voice/stt`, `voice/hotkey`, and the top-level
  `VoiceManager` enabled-flag persistence.
- 4 manifest **integration tests** in `tests/voice_manifest.rs`
  exercise the Ed25519 signing pipeline with freshly-generated keys
  (the compile-time pinned pubkey is non-functional in dev so the
  pubkey-explicit `verify_signature_with_key` is the test injection
  point).
- **TypeScript** covered by `tsc -b` strict mode + `vite build` (51+
  modules transformed cleanly).
- **e2e gate**: `scripts/e2e/v1-d1-voice.sh` — frontend build, voice
  test sweep, fmt/clippy, doctor regression check.

## What's deferred to v1.0 release / v2

- **M1.6.4 bench harness** — first-byte latency + RTF measurements.
  Code skeleton lands when the ONNX + GGUF fixtures arrive.
- **Voice cloning** — user-uploaded reference audio with AudioSeal
  watermark + ToS gating (v2).
- **Streaming voice** — interruptible barge-in via Silero VAD (v2).
- **Voice picker download UI** — surface the 4 non-default voices for
  one-click download. Backend (`voice_download`) is shipped; UI is a
  small follow-up.
- **First-memory onboarding wizard** — D2 (M2).
