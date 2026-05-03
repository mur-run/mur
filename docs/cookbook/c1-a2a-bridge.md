# Track C1 — A2A Bridge Architecture

> A chat-platform bridge is **a small, dumb mur agent** with `entitlements.llm.mode = off`. It signs every outbound A2A envelope; the user agent pins the bridge's pubkey in `profile.yaml.trusted_peers[]` and rejects everything else.

Concrete platforms (Telegram → C2, send-from-any-app → C3) build on this pattern.

## Why a bridge is a mur agent

| Alternative | Rejected because |
|---|---|
| Library linked into user agent | Couples Slack outage to therapy |
| Python sidecar that pokes user-agent HTTP API | Re-implements auth, secrets, telemetry, lifecycle |
| Smart bridge w/ LLM triage | +800 ms; social-engineerable; breaks 99.99% target |

So a bridge is just another P0a runtime — `mur_agent_<platform>_inbound` — with `llm.mode = off`, its own Ed25519 identity, and the same `running.lock` + telemetry + permissions infra as any other agent.

## Wire shape

```rust
pub struct SignedEnvelope {
    pub payload: Vec<u8>,                  // canonical-JSON A2A JsonRpcRequest
    pub sig: Vec<u8>,                      // 64-byte Ed25519
    pub key_version: u32,
    pub bridge_pubkey_multibase: String,
}
```

Verification runs **regardless of transport** — Unix socket has no peer auth; Noise XK only proves *some* peer's identity, not authorization to claim the bridge role.

## `routes.yaml`

```yaml
default_route: coach
routes:
  - match: { platform: telegram, mention: "@coach" }
    agent: coach
  - match: { platform: telegram, chat_id: "12345" }
    agent: therapist
  - match: { platform: telegram, chat_id: "67890" }
    agent: coach
    fanout: [coach, journal_agent]
```

Precedence: mention > chat_id > `default_route`. No LLM in routing.

## Behaviour summary

- **Dedupe** `(bridge_id, platform_msg_id)` → sled, 7-day TTL, lazy sweep every 256 lookups. (`DedupeStore`)
- **ACK** Bridge advances its platform offset only on 2xx. On 5xx the offset stays pinned; dedupe drops the re-fetched duplicates. (`AckTracker`)
- **Heartbeat** `telemetry/bridge_alive` every 30 s. `mur agent doctor` shows `degraded` once `running.lock` mtime > 90 s. (`BridgeBeacon`, `bridge_status_for_peer`)

## Scaffolding a stub bridge (testing only)

```bash
mur agent companion connector add stub_bridge \
    --platform stub \
    --default-route coach
```

Writes `~/.mur/agents/stub_bridge/{profile.yaml,routes.yaml,identity.{key,pub},sys_prompt.md}`. The user agent must then add the bridge pubkey to its own `trusted_peers[]` (manual YAML edit for now; CLI sugar lands in C2).

## NOT in C1

- No Telegram / Slack / Discord / IMAP → C2 / C3
- No `send-from-any-app` UX → C3
- No CLI sugar for `add-trusted-peer` → C2
