# Track C5 — Webhook Receiver Design

**Date:** 2026-05-05
**Status:** draft
**Roadmap:** §5.6 (deferred from v1; first v2 entry)

## 1. Problem

Track C3 lit up four send-from-any-app channels for the user's local machine. Track C5 extends "send" beyond the local box: any external system that can issue an HTTP POST should be able to deliver text / URLs / files into a running mur agent's inbox, with HMAC auth and the same B0 `<untrusted_share>` wrapping the local channels get.

Concrete use cases:

- **GitHub Action** → curl posts a build result summary to `https://my-mac.tailscale/agents/coach/webhook` so the agent sees the failure on the next turn
- **n8n / Zapier flow** → posts a Notion page diff to the agent
- **iOS Shortcuts** → POST share-sheet output (when the user is away from their Mac and can't use Track C3)
- **A second mur-commander instance** → cross-host agent-to-agent via plain HTTP fallback when the C1 Noise XK transport isn't reachable

## 2. Non-goals

- Public-internet exposure. Webhooks bind to localhost or a Tailscale / VPN address. Apple / Google / public cloud onboarding is out of scope; if the user wants public reachability they SSH-tunnel or set up their own ingress.
- Streaming / WebSocket. POST one payload, get one ack. Streaming joins the C9 (Telegram Mini App / cross-host A2A) work.
- Webhook **outbound** (the agent calling external HTTP endpoints). MCP already covers that surface.

## 3. Architecture

### 3.1 Where it lives

`mur-agent-runtime/src/transport/webhook.rs` — sibling to the existing Noise XK TCP listener (`transport/tcp.rs` from P0a.5). Each agent that opts in starts a single Axum server alongside its other transports.

The supervisor decides whether to start the listener based on `profile.yaml`'s `transport.webhook.{enabled, bind, port, hmac_secret_ref}` block — same shape as `transport.tcp` from P0a.5. Default off.

### 3.2 Endpoint shape

```
POST /agents/<slug>/webhook
Headers:
  X-Mur-Signature: sha256=<hex hmac>
  Content-Type: application/json
Body (JSON):
  {
    "kind": "text" | "url" | "image" | "file",
    "value": <string for text/url; base64 for image/file>,
    "metadata": { "source_hint": "github-actions", ... }   // optional, free-form
  }
Response:
  202 Accepted {"id": "<sha256 of body>", "queued_at": "<rfc3339>"}
  401 Unauthorized   — missing or wrong signature
  413 Payload Too Large — body > 10 MiB
  415 Unsupported Media Type — bad kind
  503 Service Unavailable — agent isn't running (lock file missing)
```

The `<slug>` in the path must match the agent's slug — multi-tenanting on a single port. When more than one agent listens on the same port (a future v2 fleet config), the path multiplexes; for now each agent binds its own port.

### 3.3 HMAC auth

`X-Mur-Signature: sha256=<hmac>` over the raw request body. Secret comes from the OS keychain via `SecretRef` (same pattern as Telegram bot tokens in C2). `mur agent webhook secret set <name>` stores it; nothing in the keychain → bind refuses to start.

Constant-time comparison (`subtle::ConstantTimeEq`). Replay protection is the user's problem; webhooks generally don't need a nonce because the sender either:
- has a unique payload per call (build-result post-mortems)
- has its own retry / idempotency layer (Zapier dedupes by request id)

If we ever need nonce protection we add `X-Mur-Request-Id` + a sled-backed seen-set. Not in v1.

### 3.4 Routing — reuse SendIngestor

The webhook payload is exactly Track C3's `SharePayload` shape minus the `source` field (which we set to `"webhook"` server-side). The handler:

1. Validates HMAC, body size, kind enum.
2. Decodes base64 if `kind ∈ {image, file}` and writes a temp file (mirrors the hotkey channel's tempfile dance).
3. Constructs `SharePayload { source: "webhook", kind, metadata }` and dispatches through the same `SendIngestor` as the local channels.
4. Returns 202 with the sha256 of the body for receipts.

No new pipeline plumbing. The webhook is just a 5th channel that happens to arrive over HTTP.

### 3.5 B0 contract

Same as Track C3:
- Body wrapped in `<untrusted_share>` on `on_prompt_submit`
- `after_untrusted_input` turn-flag set → Rule-4 cooldown denies side-effect tools for one turn
- Provenance entry tagged `source: "webhook"` so the user can audit which path delivered the bytes

The user telling the agent "post this back to the webhook sender" doesn't need any new wiring — they configure an MCP HTTP outbound for the sender's API and the existing B0 secret-prefilter (rule 7) gates the egress.

## 4. Milestones (planned cascade)

| # | Milestone | Scope |
|---|---|---|
| **M5.1** | Webhook config schema | `transport.webhook` block in `AgentProfile`; `mur agent webhook enable/disable/secret set` CLI |
| **M5.2** | `transport/webhook.rs` Axum handler | POST handler + HMAC verifier + body decoder; pure unit tests against synthetic Axum requests |
| **M5.3** | Supervisor wiring | Start the Axum server when `transport.webhook.enabled`; bind to localhost by default; surface errors at boot |
| **M5.4** | SendIngestor bridge | Hand off to the same multimodal pipeline path Track C3 uses (`process_share_text` / `process_artifact`) |
| **M5.5** | Rate limit | Per-source token bucket (default: 60/min per `X-Mur-Source` header or remote IP) so a misbehaving sender doesn't flood B0 |
| **M5.6** | E2E + cookbook | `scripts/e2e/c5-webhook.sh` — runs Axum locally, posts a signed payload, asserts it lands in `inputs.jsonl`. Cookbook covers GitHub Action / curl examples. |

## 5. Acceptance

```bash
mur agent webhook enable coach --port 6789
mur agent webhook secret set coach   # prompts for hmac key
curl -X POST http://localhost:6789/agents/coach/webhook \
    -H "X-Mur-Signature: sha256=$(echo -n '{"kind":"text","value":"hello"}' | openssl dgst -sha256 -hmac "$SECRET" -binary | xxd -p -c 256)" \
    -H 'Content-Type: application/json' \
    -d '{"kind":"text","value":"hello"}'
# → 202 {"id":"...","queued_at":"..."}
```

Coach's next turn sees the text wrapped in `<untrusted_share>` with `source="webhook"`, and side-effect tools are gated for one turn.

## 6. Open questions (defer to plan)

- **Port allocation:** static per-agent (config) vs. mDNS auto-discovery. Static for v2; mDNS later.
- **TLS:** rely on Tailscale / VPN; we don't terminate TLS in the agent. If the user wants HTTPS they front-proxy.
- **Multipart for files:** v1 sticks with base64 in JSON; bigger files (>10 MiB) need a follow-up that streams directly to the multimodal pipeline.
- **Authentication beyond HMAC:** mTLS / OAuth2 are out-of-scope for v2 first cut.
