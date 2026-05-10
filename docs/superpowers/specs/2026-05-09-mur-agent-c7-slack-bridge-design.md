# mur Agent C7 — Slack Bridge Design

**Status:** Approved — ready for implementation plan.
**Date:** 2026-05-09.
**Authors:** David + Claude (Sonnet 4.6).
**Predecessors:**
- [`2026-04-30-mur-agent-harness-roadmap-design.md`](./2026-04-30-mur-agent-harness-roadmap-design.md) — roadmap §5 Track C (shipped C1-C6).
- C1 A2A bridge (shipped) — DedupeStore, AckTracker, SignedEnvelope, BridgeBeacon, BridgeRouteConfig.
- C2 Telegram bridge (shipped) — reference template for bridge pattern, InboundLoop, MockBot.

---

## 0. Executive Summary

C7 ships a Slack bridge agent using the C1 foundation pattern: a zero-LLM mur agent that connects to Slack via **Socket Mode** (WebSocket, no public URL required), deduplicates events, signs envelopes with Ed25519, forwards them to the user's mur agent via A2A, and posts replies via `chat.postMessage`. The bridge handles **channel @mentions** (replies in-thread) and **DMs** (inline reply). One shared agent serves all workspace members.

No new top-level Cargo dependencies: `tokio-tungstenite` and `reqwest` are already workspace members.

---

## 1. Use Case & Scope

### 1.1 Primary Use Case

**Team sharing**: a single mur agent is deployed as a Slack app in a workspace. Any workspace member can trigger the agent by:
- Mentioning `@<bot-name>` in a channel the bot has been invited to.
- Sending the bot a direct message.

The agent replies in-thread for channel mentions (to keep channels clean) and inline for DMs.

### 1.2 Out of Scope

- Per-member agent routing (C7 uses a single shared agent; C1 `BridgeRouteConfig` mention-routing is a future extension).
- Events API / HTTP webhook mode (Socket Mode only; no public URL required).
- Slash commands (`/mur …`) — deferred to C7 v2.
- File upload from agent → Slack — deferred to C7 v2.
- Slack blocks / rich UI — text-only replies in v1.

---

## 2. Architecture

```
Slack workspace
  │  Socket Mode WSS (wss://wss-...)
  ▼
SlackSocketConn              ← tokio-tungstenite WebSocket
  │  events_api envelopes
  ▼
SlackInboundLoop<B: SlackBotLike>
  ├─ PrivacyGate             ← DmOnly | DmAndMentions + allowed_channels whitelist
  ├─ DedupeStore             ← (bridge_id, "{channel}:{ts}") — 7d TTL, reused from C1
  ├─ SignedEnvelope           ← Ed25519, reused from C1
  ├─ A2A message/send        → user agent
  ├─ AckTracker              ← advances offset only on 2xx, reused from C1
  └─ chat.postMessage        ← reply helper in reply.rs
BridgeBeacon                 ← 30 s heartbeat, reused from C1 unchanged
```

### 2.1 Token Model

Two secrets stored in system keychain (never in profile.yaml):

| Token | Prefix | Purpose | Keychain account |
|-------|--------|---------|-----------------|
| App Token | `xapp-` | `apps.connections.open` → WSS URL | `mur_slack_app_<agent>` |
| Bot Token | `xoxb-` | `chat.postMessage`, `auth.test` | `mur_slack_bot_<agent>` |

### 2.2 Socket Mode Protocol

```
POST apps.connections.open (App Token)
  → { url: "wss://wss-primary.slack.com/…?ticket=…" }

WebSocket connect(url)
  ← { type: "hello", num_connections: 1 }

loop:
  ← { type: "events_api", envelope_id: "Ev…", payload: { event: {…} } }
  → { envelope_id: "Ev…" }   // ACK within 3 s (Slack retries on timeout)

on close:
  exponential backoff: 1s → 2s → 4s → … → max 60s → reconnect
```

---

## 3. Components

### 3.1 File Map

```
mur-common/src/bridge/
  slack_config.rs            CREATE — SlackConfig + SlackPrivacyMode enums

mur-agent-runtime/src/bridge/slack/
  mod.rs                     CREATE — pub use + feature docs
  socket.rs                  CREATE — SlackSocketConn (WSS lifecycle + reconnect)
  inbound.rs                 CREATE — SlackInboundLoop<B: SlackBotLike> + InboundDeps
  reply.rs                   CREATE — post_message() → chat.postMessage
  mock.rs                    CREATE — MockSlackBot + MockSlackMessage for tests

mur-agent-runtime/tests/
  c7_slack_inbound.rs        CREATE — pipeline tests (dedupe, privacy, routing, reply)
  c7_slack_socket.rs         CREATE — reconnect + ACK tests

scripts/e2e/
  c7-slack-bridge.sh         CREATE — mock-mode E2E runner

docs/cookbook/
  c7-slack-bridge.md         CREATE — user-facing setup + usage guide
```

### 3.2 SlackConfig

```rust
// mur-common/src/bridge/slack_config.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Human-readable Slack workspace URL (for UI / error messages only).
    pub workspace_url: String,
    /// Keychain account name for the xoxb- Bot Token.
    pub bot_token_keychain_account: String,
    /// Keychain account name for the xapp- App Token.
    pub app_token_keychain_account: String,
    /// Privacy gate mode.
    pub privacy_mode: SlackPrivacyMode,
    /// Allowed Slack channel IDs (Cxxxxxxxx). Empty = all channels allowed.
    pub allowed_channels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SlackPrivacyMode {
    DmOnly,
    #[default]
    DmAndMentions,
}
```

Profile snippet written by setup wizard:
```yaml
bridge:
  platform: slack
  workspace_url: "https://yourworkspace.slack.com"
  bot_token_keychain_account: "mur_slack_bot_my-agent"
  app_token_keychain_account: "mur_slack_app_my-agent"
  privacy_mode: dm_and_mentions
  allowed_channels: []
```

### 3.3 SlackError

Local error enum defined in `mur-agent-runtime/src/bridge/slack/mod.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SlackError {
    #[error("auth error (HTTP {0})")]
    Auth(u16),
    #[error("rate limited; retry after {0:?}")]
    RateLimit(std::time::Duration),
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
}
```

### 3.4 SlackBotLike Trait (for mocking)

```rust
#[async_trait::async_trait]
pub trait SlackBotLike: Send + Sync {
    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<(), SlackError>;

    async fn auth_test(&self) -> Result<SlackUser, SlackError>;
}
```

`RealSlackBot` wraps a `reqwest::Client` + `xoxb-` token.
`MockSlackBot` records calls into `Mutex<Vec<MockSlackMessage>>`.

### 3.4 SlackSocketConn

```rust
pub struct SlackSocketConn {
    app_token: String,          // xapp-
    reconnect_backoff: Duration, // current backoff, reset on successful hello
}

impl SlackSocketConn {
    /// Open connection: POST apps.connections.open → WSS URL → connect.
    pub async fn connect(&mut self) -> Result<WssStream, SlackError>;
    /// Run one connection lifetime: read events → yield SlackEnvelope.
    pub async fn run_loop(
        &mut self,
        tx: mpsc::Sender<SlackEnvelope>,
    ) -> Result<(), SlackError>;
}
```

Reconnect strategy: on any `WsError` or `SlackError::Auth` the loop returns; the supervisor restarts with exponential backoff (1 s doubling, cap 60 s). `SlackError::Auth` (401 from `apps.connections.open`) stops retrying and logs a clear error.

---

## 4. Message Pipeline

`SlackInboundLoop::tick_once(envelope: SlackEnvelope)`:

```
1. Parse event type:
   - "app_mention"                             → channel mention
   - "message" + channel_type == "im"          → DM
   - anything else                             → skip (ACK still sent)

2. PrivacyGate:
   - DmOnly + channel mention                  → skip
   - allowed_channels non-empty + channel not in list → skip

3. DedupeStore.check(bridge_id, "{channel}:{ts}")
   - seen                                      → skip (ACK sent to suppress Slack retry)
   - new                                       → record + continue

4. Build A2A payload:
   - text: event.text (strip bot mention prefix "<@U…> ")
   - sender_slack_user_id: event.user
   - channel: event.channel
   - ts: event.ts
   - thread_ts: event.thread_ts (if present)

5. SignedEnvelope::sign(payload, bridge_keypair)

6. A2A message/send → user agent
   - 2xx → AckTracker.advance()
   - 5xx → log + keep offset (will retry on reconnect)

7. Extract reply text from A2A response body

8. chat.postMessage:
   - channel: event.channel
   - text: reply_text
   - thread_ts:
       mention  → event.ts   (start/continue thread)
       DM       → None       (inline)

9. Send envelope ACK { envelope_id }
   (ACK is always sent regardless of postMessage outcome,
    to prevent Slack from retrying the event)
```

**ACK / reply decoupling**: the 3-second Slack ACK deadline is met by sending ACK immediately after step 9; `postMessage` runs concurrently in a spawned task. If `postMessage` fails, a warning is logged and the error is swallowed (Slack already saw ACK — no retry path).

---

## 5. Reply Posting

```rust
// reply.rs
pub async fn post_message(
    client: &reqwest::Client,
    bot_token: &str,
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
) -> Result<(), SlackError>
```

Rate limit handling: on HTTP 429, read `Retry-After` header (seconds), sleep, retry up to 3 times. After 3 failures, log error and continue (message is dropped rather than blocking the event loop).

---

## 6. Setup UX

```
$ mur agent companion connector add --platform slack my-agent
```

Interactive 5-step wizard (mirrors C2 BotFather UX):

```
━━ Slack Bridge Setup ━━━━━━━━━━━━━━━━━━━━━━━━━━━

Step 1/5  Create a Slack App
          → https://api.slack.com/apps → Create New App → From scratch
          Name: anything (e.g. "my-agent"); Workspace: your team's workspace
          Press Enter when done…

Step 2/5  Enable Socket Mode + App Token
          Settings → Socket Mode → Enable Socket Mode
          → Generate an App-level Token with scope: connections:write
          → Copy the token (starts with xapp-)
          App Token: ____

Step 3/5  Add Bot Token Scopes
          OAuth & Permissions → Bot Token Scopes → Add:
            app_mentions:read  im:read  im:history
            chat:write  users:read  channels:read
          Press Enter when done…

Step 4/5  Install App + Bot Token
          OAuth & Permissions → Install to Workspace → Allow
          → Copy the Bot OAuth Token (starts with xoxb-)
          Bot Token: ____

Step 5/5  Verifying… auth.test ✓  Socket Mode connection ✓

          ✅ Slack bridge configured for agent 'my-agent'.
          Run: mur agent start my-agent

━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

⚠ Privacy notice: This bridge is NOT end-to-end encrypted.
  Messages are forwarded to your local mur agent over A2A.
```

Tokens stored in system keychain. `mur agent companion connector add` extends the existing C2 `--platform telegram` dispatch to include `--platform slack`.

---

## 7. Error Handling

| Scenario | Behavior |
|----------|----------|
| WebSocket close (network drop) | Exponential backoff reconnect: 1s → 2s → 4s → … → 60s cap |
| `apps.connections.open` 401 | Stop reconnecting; log clear error; BridgeBeacon stops (supervisor detects degraded state) |
| Slack ACK timeout (>3s) | Slack auto-retries; DedupeStore blocks duplicate processing |
| `chat.postMessage` 429 | Read `Retry-After`, sleep, retry ≤3 times; then log + continue |
| A2A user agent 5xx | AckTracker holds offset; retry after reconnect |
| Bot token revoked | `auth.test` returns 401 → log actionable message + stop |

---

## 8. Testing Strategy

### Unit / Integration Tests

| Test | Verifies |
|------|----------|
| `mention_routes_to_agent` | app_mention → A2A sent + thread reply (thread_ts = event.ts) |
| `dm_routes_to_agent` | im message → A2A sent + inline reply (no thread_ts) |
| `duplicate_event_skipped` | same channel:ts twice → DedupeStore blocks second A2A |
| `dm_only_drops_mention` | privacy=DmOnly + channel mention → skipped |
| `allowed_channels_gate` | channel not in whitelist → skipped |
| `a2a_5xx_holds_offset` | agent returns 500 → AckTracker does not advance |
| `rate_limit_retry_succeeds` | postMessage 429 → sleep → retry → success |
| `reconnect_on_ws_close` | WebSocket close → backoff → reconnect (mock WSS server) |
| `envelope_signed_correctly` | Ed25519 signature verification passes on receiver side |
| `mention_prefix_stripped` | "<@U12345> help" → A2A payload text is "help" |

### E2E

`scripts/e2e/c7-slack-bridge.sh --self-test=mock` — spins up a mock WSS server + mock user agent, replays 3 event types (mention, DM, duplicate), verifies ACK sequence and reply content.

---

## 9. Acceptance Criteria

- [ ] `cargo test -p mur-agent-runtime --test c7_slack_inbound` — all green
- [ ] `cargo test -p mur-agent-runtime --test c7_slack_socket` — all green
- [ ] `cargo clippy --workspace -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] `mur agent companion connector add --platform slack <name>` 5-step wizard completes and stores tokens in keychain
- [ ] `mur agent start <name>` — DM to bot → agent replies within 3 s
- [ ] Channel `@mention` → agent replies in thread (not in-channel)
- [ ] `BridgeBeacon` heartbeat appears in telemetry every 30 s
- [ ] `scripts/e2e/c7-slack-bridge.sh --self-test=mock` passes
- [ ] `docs/cookbook/c7-slack-bridge.md` covers full setup + privacy disclosure

---

## 10. Decision Log

| # | Decision | Rationale |
|---|----------|-----------|
| 1 | Socket Mode (not Events API) | No public URL needed; local-first mur philosophy |
| 2 | Single shared agent (not per-member routing) | Simplest model; BridgeRouteConfig mention-routing deferred to v2 |
| 3 | Custom WebSocket (not slack-morphism) | Zero new top-level deps; tokio-tungstenite already in workspace |
| 4 | Channel mentions reply in-thread | Slack best practice for bots; avoids channel noise |
| 5 | DM replies inline (no thread_ts) | Natural DM UX; threads in DMs are awkward |
| 6 | ACK sent after postMessage in spawned task | Meets 3s Slack deadline without blocking |
| 7 | Bot + App tokens in keychain only | Same security posture as C2 Telegram token storage |
| 8 | Slash commands deferred to v2 | Avoids `commands` scope + Slack manifest complexity in v1 |

---

## 11. Roadmap Footer

§5.7 C7 ship status: **shipped** (this document). PRs M-c7.0 through M-c7.7 merged.

Next: `docs/cookbook/c7-slack-bridge.md`
