# D1 Voice — Kokoro TTS + whisper.cpp STT

On-device voice I/O for mur agents. Kokoro 82M synthesises speech
locally at 24 kHz; whisper.cpp large-v3-turbo q5_1 transcribes mic
audio at 16 kHz. No audio or transcript leaves the device.

## Quick start

```bash
# Enable voice on an agent (sets profile.voice.enabled = true).
mur agent voice enable my-agent

# Optional: choose a voice (default: af_heart).
mur agent voice enable my-agent --voice-id am_michael

# Download the model weights (~1.4 GB total; SHA-256 verified).
mur agent voice download my-agent

# Disable voice.
mur agent voice disable my-agent
```

## Available voices

| Voice ID | Description |
|---|---|
| `af_heart` | Female, warm American English (default) |
| `af_bella` | Female, bright American English |
| `af_nicole` | Female, neutral American English |
| `am_adam` | Male, neutral American English |
| `am_michael` | Male, deeper American English |

## Model locations

| Model | Path | Size |
|---|---|---|
| whisper large-v3-turbo q5_1 | `~/.mur/models/whisper/ggml-large-v3-turbo-q5_1.bin` | ~930 MB |
| Kokoro ONNX | `~/.mur/models/kokoro/kokoro-v0_19.onnx` | ~85 MB |
| Kokoro style matrix | `~/.mur/models/kokoro/kokoro-voices.bin` | ~5 KB |

Models are cached permanently. Re-running `voice download` is a
no-op if SHA-256 matches. To force re-download, delete the file.

## How it works

### TTS (companion outbox)

When voice is enabled, the companion outbox wires a `VoiceNotifier`
at step 11 instead of (or alongside) `StdoutNotifier`. Each proactive
companion message is synthesised by Kokoro and played on the default
output device before the inbox `.md` file is written.

### STT (mic → agent input)

`VoiceInputHook` fires on every `on_prompt_submit`. It captures
audio from the default input device, applies a simple RMS voice
activity detector, and transcribes with whisper.cpp. The transcript
is injected as an `UntrustedWrapper`:

```
<untrusted_voice_input>
{transcript text}
</untrusted_voice_input>
```

This is B0 rule 18 — voice input is treated as untrusted, identical
to drag-drop text (D3) and Telegram messages (C2). The agent sees
the wrapper and knows the text came from a mic, not the keyboard.

### Privacy invariant

Voice audio is processed entirely on-device:
- Kokoro TTS: ort ONNX Runtime, no network calls.
- whisper.cpp: `whisper-rs` Rust bindings over the local C++ lib.
- `cpal` audio I/O: no network.

The compile-time `voice::network_audit` test fails the build the
moment any voice module imports `reqwest`, `hyper`, or any
`tokio::net::*` type.

## Troubleshooting

**No audio output / input:** run `mur agent voice list-devices` to
see what cpal sees. On macOS, grant microphone permission in System
Settings → Privacy & Security.

**Poor transcription:** the large-v3-turbo model performs best with
clear speech in a quiet room. VAD threshold defaults to RMS 0.01;
lower if it cuts off your voice early.

**Kokoro sounds robotic on a phoneme:** this is a known gap in the
v1 phoneme vocabulary (`PHONEME_VOCAB` partial map). File an issue
with the word and the IPA espeak-ng output; it will be added to the
next release's vocab table.

## See also

- `mur-agent-runtime/src/voice/` — implementation
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §4.1 — spec
- `docs/superpowers/plans/2026-05-06-mur-agent-d1-voice.md` — this plan
