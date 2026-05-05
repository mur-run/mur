# B0 telemetry redaction (rule 9 + rule 12 audit)

Closes the last v1 privacy-correctness gaps. Tells you what mur writes
to disk, what gets redacted, and what doesn't — so you can write a
release-time privacy statement that's actually true.

## What gets written

Every running agent appends a JSONL line per event to:

```
~/.mur/agents/<name>/telemetry/<YYYY-MM-DD>.jsonl
```

The event variants are documented in `mur-agent-runtime/src/telemetry_writer.rs`.
The structural fields are intentional metadata:

| Field | Source | Always written? |
|---|---|---|
| `gen_ai.usage.input_tokens` / `…output_tokens` | M0 hook chain | yes |
| `gen_ai.request.model` | hook chain | yes |
| `mur.task.id` | task runner | yes |
| `mur.mcp.server` / `tool` | hook chain | yes |
| `latency_ms` / `cost_usd` / `duration_ms` / `ok` | hook chain | yes |
| `Error.message` / `Warning.message` / `TaskProgress.message` | free-form | yes — **redacted** |
| `HookFired.attrs` (object merged into envelope) | arbitrary hook | yes — **redacted** |

## What gets redacted (rule 9 / M8.1)

A single chokepoint inside the writer applies two redactor passes to
every JSON string leaf before it hits the disk and before it's
forwarded to a transport subscriber.

### Credential redactor

Reuses the M7.5 outbound-secret regex set (rule 7). Both the rule-7
"drop the message" path and the rule-9 "scrub on write" path share
one source of truth, so the patterns can never drift:

| Pattern | Replaced with |
|---|---|
| `sk-…` (≥ 20 alphanumeric) | `[REDACTED:openai_key]` |
| `sk-ant-…` | `[REDACTED:anthropic_key]` |
| `AKIA…` (16 uppercase alnum) | `[REDACTED:aws_access_key]` |
| `aws_secret_access_key=…` (40-char base64) | `[REDACTED:aws_secret_key]` |
| `ghp_…` / `ghs_…` (36-char) | `[REDACTED:github_pat]` / `…app_token` |
| `AIza…` (35-char) | `[REDACTED:gcp_api_key]` |
| `eyJ….….…` (JWT) | `[REDACTED:jwt]` |
| `-----BEGIN … PRIVATE KEY-----` | `[REDACTED:pem_private_key]` |
| `hooks.slack.com/services/…` | `[REDACTED:slack_webhook]` |
| `(api_key\|token\|password)=<20+ chars>` | `[REDACTED:env_assignment]` |

### Home-path redactor

Collapses OS-account-name path prefixes so error messages don't leak
the local username:

| Input | Output |
|---|---|
| `/Users/alice/secret.txt` | `~/secret.txt` |
| `/home/bob/.ssh/id_rsa` | `~/.ssh/id_rsa` |
| `C:\Users\Carol\Desktop\notes.md` | `~\Desktop\notes.md` |

The trailing path is preserved so debug context survives.

## What does NOT get redacted

These are operational telemetry and are part of the v1 contract:

- token counts, latency, cost
- model names (`claude-opus-4-7`, `gpt-4o`, `llama3:8b`)
- MCP server names + tool names
- task IDs / trace IDs / agent UUID
- timestamps, durations, `ok` boolean

If you're worried about model-name disclosure (e.g. you're testing a
private fine-tune), set the model alias in `~/.mur/models.yaml` and the
alias name is what telemetry sees.

## Companion subsystem (rule 12 audit / M8.3)

The companion subsystem (`mur-agent-runtime/src/companion/`) has **no
direct network egress** by construction. The audit at
`mur-agent-runtime/src/companion/network_audit.rs` runs at every test
invocation:

1. Embeds every companion source via `include_str!`.
2. Asserts none of them imports `reqwest`, `hyper`, `surf`, `ureq`,
   `isahc`, `tokio::net::*`, `std::net::*`, or any raw socket type.
3. Drift guard: `pub mod` count in `companion/mod.rs` must match the
   audit's file list — adding a new companion file silently fails CI
   until it's listed in the audit.
4. Allow-list check: every `crate::llm::*` reference must resolve to
   the LlmClient surface (`LlmClient`, `LlmError`, `LlmMessage`,
   `LlmRequest`, `LlmResponse`) or a recognised provider sub-module
   (`anthropic`, `ollama`, `openai`, `stub`).

The single allowed outbound is `crate::llm::LlmClient` — the same
model-provider call the agent already makes for any tool execution,
which the user opted into when they chose the model. There is **no
companion-specific egress beyond what the rest of the agent already
does**.

## How to verify on your machine

Run the harness-level acceptance:

```bash
bash scripts/e2e/b0-m8-telemetry-redaction.sh
```

For a real-bundle check, start an agent and intentionally inject a
secret pattern through any tool call that lands in `HookFired.attrs`,
then tail the JSONL:

```bash
mur agent send my-agent "test message: sk-ant-abcdefghijklmnop-9999"
sleep 2
tail -f ~/.mur/agents/my-agent/telemetry/$(date -u +%Y-%m-%d).jsonl | jq .
# Expect: any string mentioning the key shows up as `[REDACTED:anthropic_key]`.
```

## How to disable telemetry entirely

Set the per-agent `telemetry.enabled` flag to `false` in
`~/.mur/agents/<name>/profile.yaml`:

```yaml
telemetry:
  enabled: false
```

This silences the writer thread; nothing is appended to
`telemetry/*.jsonl`. The hook chain still runs (B0 rules 1–22 still
fire) — only the on-disk record is suppressed.

## What still leaves the device

Telemetry redaction does NOT cover what the agent itself sends to its
configured model provider over the network. That's the agent's normal
operation, governed by:

- `entitlements.network.outbound.allowlist` (rule 2) — host-level gate
- B0 rule 7 outbound credential pre-filter — drops the entire message
  if a secret is detected

For the privacy statement (`docs/release/privacy-statement.md`, M8.5)
the precise claim is:

> The agent only sends content over the network to:
>   1. The model provider you configured (Anthropic / OpenAI / Ollama / etc.)
>   2. MCP servers you explicitly added with `mur agent mcp add`
>   3. Bridges you explicitly enabled (Telegram, webhook, …)
>
> All other network paths are blocked. The companion subsystem in
> particular has no direct outbound; its only network call is to the
> agent's configured model provider, identical to any tool-driven
> model call.

## See also

- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.1 (B0 rules 9 + 12)
- `docs/superpowers/specs/2026-05-05-b0-m8-telemetry-hardening-design.md` (M8 design)
- `mur-agent-runtime/src/hooks/b0_helpers.rs` (`scan_for_secrets`, `redact_secrets`, `redact_home_path`)
- `mur-agent-runtime/src/telemetry_writer.rs` (`redact_envelope`)
- `mur-agent-runtime/src/companion/network_audit.rs` (rule 12 enforcement)
