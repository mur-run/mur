# Track C2 — Telegram Reference Bridge

> Telegram is the v1 reference chat platform. The bridge is **a small mur
> agent** with `entitlements.llm.mode = off` that long-polls the Telegram
> Bot API, signs every inbound A2A envelope, and forwards to the user
> agent. The user agent calls back via the bridge's outbound MCP server
> (`chat.send_message`) to reply.

This cookbook walks through the v1 setup, documents the privacy
trade-offs, and lists what is intentionally **NOT** in v1.

## Setup (5 steps)

| Step | Action |
|------|--------|
| 1 | Open `https://t.me/BotFather` in Telegram, send `/newbot`, follow the prompts (display name, then username ending in `bot`). |
| 2 | Copy the bot token (`123456:ABC-DEF...`) BotFather emits. Treat it like an API key. |
| 3 | Run `mur agent companion connector add tg --platform telegram --default-route <user-agent>`. The CLI opens a BotFather URL, prompts for the token, and walks the disclosure ack. |
| 4 | The CLI prints `https://t.me/<bot_username>?start=<nonce>`. Tap it on your phone, hit **Start**, and paste the resulting `chat_id` back into the CLI. |
| 5 | The CLI writes `~/.mur/agents/tg/telegram.yaml`, stores the bot token in the OS keychain (account `tg/telegram_bot_token`, service `mur-agent`), and prints the E2E disclosure one last time. |

For tests / CI / scripted setup, use the non-interactive flag path:

```bash
MUR_TELEGRAM_KEYCHAIN_BACKEND=mock \
    mur agent companion connector add tg \
        --platform telegram \
        --default-route coach \
        --bot-token "123:fake" \
        --bot-username MyAgentBot \
        --chat-id 100 \
        --ack
```

`--ack` is mandatory — the scaffold refuses to write any state without
explicit E2E disclosure acknowledgement.

## Privacy mode trade-offs

| Mode | What flows in | When to use |
|------|---------------|-------------|
| `DmOnly` (default) | Only direct messages from the bound `chat_id` | The 1-user / 1-agent case. Group messages are dropped silently — they never reach the user agent. |
| `AllowGroups` | DMs + listed group `chat_id`s | Coach / journal that you want to mention in a couples or family group chat. Set via `--allow-group <id>,<id>` at scaffold time. |

**Default ON privacy mode** is enforced **client-side at BotFather**
(`/setprivacy → ENABLED`) so the bot only sees `@mention` and replies in
groups, AND **enforced again at the bridge** so a privacy-mode regression
on Telegram's side (or accidental BotFather mis-configuration) does not
leak group traffic to the user agent.

## Per-channel UX

| Channel | Inbound path | Outbound path |
|---------|--------------|---------------|
| Text | long-poll → dedupe → sign → A2A `message/send` to user agent | user agent → MCP `chat.send_message {chat_id, body}` → teloxide `Throttle` → Telegram |
| Voice (`.ogg` Opus) | `getFile` → local `whisper-rs` transcription → `<voice_transcript>` wrapper → A2A | (same as text; user agent's reply is text-only in v1) |
| Photo / document (≤ 20 MB) | `getFile` → D3 multimodal pipeline (decode + re-encode + EXIF strip + OCR pre-pass) → `<image_ocr>` / `<pdf_text>` wrapper → A2A | (same; v1 does not send photos back) |

All inbound multimodal content goes through the **same B0 pipeline** as
drag-and-drop and character-card import (M3, M4) — nothing platform-
specific, no parallel hardening to maintain.

## Why local `whisper-rs`

Voice transcription **stays on the box**:

- No audio bytes leave the laptop. No third-party STT provider sees a
  word.
- Works offline (the moat: subway, plane, field work).
- Aligns with D1's "voice never leaves this Mac" privacy story — the
  bridge does not get a different deal than the GUI.
- Latency is acceptable for chat-pace voice messages (under 2s on M-
  series for ≤ 30s clips with the `base.en` model).

The bridge does NOT call `whisper-rs` directly. It calls
`mur_core::companion::voice::transcribe_ogg`, the same path D1 uses, so
both surfaces share one model load + one config knob.

## Rate-limit invariants

Teloxide's `Throttle` adapter wraps every outbound call:

- **Global ceiling:** 30 messages/sec across the entire bot token.
- **Per-chat ceiling:** 1 message/sec to any one `chat_id`.
- Excess requests block (token-bucket with 30-slot capacity); they are
  not dropped.
- The user-agent side does not need to rate-limit — the bridge's MCP
  server back-pressures via slow `chat.send_message` returns.

Documented hard ceilings (Telegram-imposed): 30 msg/s overall, 20 msg/min
per group, 1 msg/s per individual chat. We sit one notch below the
group ceiling intentionally.

## Heartbeat + observability

The bridge writes `telemetry/bridge_alive` every 30 s. Pair this with the
generic `BridgeBeacon` infrastructure from C1:

- `mur agent doctor --format all` shows `tg: running` (or `degraded`
  once `running.lock` mtime > 90 s).
- The user agent can call `bridge_status_for_peer("tg")` before
  composing a reply — handy for "you're offline; I'll deliver this when
  you reconnect" UX.

## What's NOT in v1

These deliberately did **NOT** ship in v1; they remain v2 work:

| Feature | Why it is deferred |
|---------|--------------------|
| **Premium Business chat** | Premium-gated; valuable as a quiet-hours auto-reply substrate but adds Premium-account dependency at the user-acquisition layer. Spec'd separately (`§5.4` C9 marker). |
| **Mini App / TWA embed** | Would require a hosted web surface, breaking the local-only invariant of v1. |
| **Inline-mode bots** (`@MyAgentBot how do I…`) | Different UX surface (inline query vs message), separate UX flow. |
| **Group admin reactions** | Only meaningful in groups; defer with the rest of the group-mode polish. |
| **Multi-bot single-chat** | One `chat_id` ↔ one bridge agent in v1. Multi-bot routing is a `routes.yaml` v2 extension. |
| **CLI sugar `mur agent companion connector token rotate`** | Lands in the C-track follow-up plan. Today, rotate manually: `mur agent secret tg delete telegram_bot_token` → re-run `connector add` with the new token. |

## Disabling

To stop a bridge:

```bash
mur agent stop tg                             # SIGTERM the runtime; running.lock clears.
mur agent secret tg delete telegram_bot_token  # Optional — purges the token from the OS keychain.
rm -rf ~/.mur/agents/tg                       # Optional — full teardown.
```

The bot token remains in the OS keychain (macOS Keychain / libsecret /
Windows Credential Manager) until you explicitly `mur agent secret
<agent> delete`. Stopping the agent only halts polling.

## See also

- [C1 — A2A Bridge Architecture](c1-a2a-bridge.md) — the underlying
  signed-envelope + dedupe + ACK substrate. C2 reuses every bit of it.
- [B0 Text-Only Safety Rules](b0-text-rules.md) — the 7 in-hook rules
  that fire on user-agent-side replies before they hit `chat.send_message`.
- [Drag-Drop Pipeline](drag-drop-pipeline.md) — D3, the multimodal
  pipeline that photos / documents / voice transcripts all flow through.
