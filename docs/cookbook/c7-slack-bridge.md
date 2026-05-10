# C7 Slack Bridge — Setup & Usage

Connect your mur agent to a Slack workspace so team members can send
messages and get replies via `@mention` or DM.

---

## Prerequisites

- A mur agent created with `mur agent create <name>`
- Admin access (or ability to create Slack Apps) in your workspace
- `mur` installed (`mur --version`)

---

## Setup

```bash
mur agent companion connector add --platform slack <agent-name>
```

Follow the 5-step interactive wizard:

1. Create a new Slack App at https://api.slack.com/apps → **From scratch**
2. **Settings → Socket Mode → Enable Socket Mode** → generate an App-level Token
   (scope: `connections:write`) → paste the `xapp-…` token
3. **OAuth & Permissions → Bot Token Scopes** → add:
   `app_mentions:read`, `im:read`, `im:history`, `chat:write`, `users:read`, `channels:read`
4. **Install to Workspace** → paste the `xoxb-…` Bot Token
5. Wizard verifies tokens via `auth.test` and writes `~/.mur/agents/<name>/slack.yaml`

---

## Starting the bridge

```bash
mur agent start <agent-name>
```

The agent supervisor starts the Slack Socket Mode listener. You should
see log lines like:

```
INFO  B0SafetyHook: B1 kernel sandbox: ENFORCING
INFO  SlackSocketConn: connected (hello received)
INFO  BridgeBeacon: heartbeat emitted
```

---

## Interacting with your agent

**In a channel (invite the bot first):**

```
/invite @<bot-name>
@<bot-name> summarise the meeting notes in #general
```

The agent replies in a thread to keep the channel clean.

**Via DM:**

Search for `@<bot-name>` and send it a direct message. The agent replies inline.

---

## Privacy & Security

> ⚠ This bridge is **not end-to-end encrypted**. Messages transit
> Slack's servers before reaching your local mur agent. Your mur agent
> runs locally; Slack cannot read the agent's memory or patterns.

Every message is signed with Ed25519 before being forwarded to the
user agent. The user agent verifies the signature against the bridge's
trusted peer list.

---

## Configuration (`slack.yaml`)

Located at `~/.mur/agents/<name>/slack.yaml`:

```yaml
workspace_url: "https://myteam.slack.com"
bot_token_keychain_account: "mur_slack_bot_myagent"   # pointer to keychain
app_token_keychain_account: "mur_slack_app_myagent"   # pointer to keychain
privacy_mode: dm_and_mentions   # dm_only | dm_and_mentions
allowed_channels: []             # [] = all; ["C111", "C222"] = allowlist
```

Tokens are stored in the system keychain, never in YAML files.

---

## Reconnection

If the WebSocket drops (network hiccup, Slack maintenance), the bridge
reconnects automatically with exponential backoff: 1s → 2s → 4s → … → 60s cap.
If the App Token is revoked (401), the bridge logs a clear error and stops — re-run
the setup wizard to issue a new token.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Bridge stops after "Auth error (401)" | App Token revoked | Re-run setup wizard |
| `chat.postMessage` rate limit in logs | Sending too fast | Bridge auto-retries with Retry-After |
| No reply in channel | Bot not invited | `/invite @<bot-name>` in the channel |
| DMs not received | Missing `im:read` scope | Reinstall app after adding scope |

---

§5.7 C7 acceptance: see `docs/superpowers/specs/2026-05-09-mur-agent-c7-slack-bridge-design.md`
