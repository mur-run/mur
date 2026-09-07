# murmur secret handoff — design

**Status:** implemented and verified on a real machine (PR #1203)
**Date:** 2026-09-07
**Branch:** `feat/murmur-secret-handoff`

## Problem

Mid-conversation, an agent needs a credential (a Gitea deploy token, an API
key). Today the only path is pasting it into the chat box. The plaintext then
lands in the LLM context, in the agent's Ed25519-signed append-only channel
(`mur-channel` performs no redaction), and at the model provider. Pattern
redaction (`mur_common::redact::redact_secrets`) matches known key shapes —
`sk-`, `ghp_`, `AKIA`, JWT, PEM — and a bare 40-hex token matches none of them.

Two structural facts shape the fix:

1. The runtime resolves provider secrets **before** the sandbox seals and
   caches them (`supervisor.rs` `cache_before_seal`, #1023/#1024). A value
   written to the keychain mid-session is invisible to the running agent
   until restart. "Write keychain, let the runtime read it" is therefore not
   a mid-conversation path.
2. The bash tool inherits the parent environment wholesale and overrides
   only `PATH` (`mur-agent-runtime/src/tools/bash.rs`). Injecting an env var
   is one `.env()` call.

## Goal

Plaintext exists only in the keychain and in runtime memory. The model sees
names only. Child processes receive the value through an environment
variable.

## Decisions (grilled, in order)

| # | Question | Decision |
|---|---|---|
| 1 | Does the model see the plaintext? | **No.** Model sees `$NAME`; runtime injects the value at spawn. Leakage becomes structurally impossible rather than filter-dependent. |
| 2 | Where does the user hand it over? | **TUI `/secret <KEY>`** with a hidden input field. Dual-write: keychain (durable) **and** A2A `secret/set` over the unix socket (reaches the running, sealed runtime). |
| 3 | How does the model learn it is available? | **System-prompt section listing names only**, refreshed on `secret/set`/`secret/delete`. TUI prints a local confirmation. No synthetic turn — same class as `/model`, `/effort`. |
| 4 | Lifetime and scope | **Persistent by default**, stored in the per-agent keychain slot `mur-agent/<agent>/<KEY>` — the same slot `mur agent secret set` uses. Strictly per-agent: never propagated to fleet members or `channel/delegate` specialists. `/secret <KEY> --delete` revokes. |
| 5 | Value masking | **One chokepoint** where tool results return to the model, covering every tool (bash, MCP, fs). Values shorter than 8 characters are rejected at `/secret`. Masking is defense in depth, not the guarantee. |
| 6 | Partial success | **Report each step separately.** Keychain failure aborts before the runtime push. Keychain ✓ + runtime not reached prints two distinct lines. Only both ✓ prints the single-line success. |

## Components and data flow

```
murmur TUI                              mur-agent-runtime
─────────                               ─────────────────
/secret GITEA_TOKEN
  │ hidden input (no echo, not in history, not sent as a message)
  │ validate: KEY matches [A-Z_][A-Z0-9_]*; value length ≥ 8
  ├─① keychain_set("mur-agent", "<agent>/GITEA_TOKEN")   ← reuses cmd_secret_set
  └─② A2A secret/set {name, value} over unix socket ───→ SecretVault (in-memory map)
                                                             │
                                          ┌──────────────────┼──────────────────┐
                                          ▼                  ▼                  ▼
                                   system prompt        bash tool spawn     tool-result chokepoint
                                   "available secrets:  .env(name, value)   value → [SECRET:NAME]
                                    GITEA_TOKEN"                            (all tools, ≥ 8 chars)

startup: supervisor loads every mur-agent/<agent>/* keychain entry into SecretVault pre-seal
```

### New

- **TUI** (`mur-core/src/cmd/agent/cli/`): `SlashCmd::Secret { key, delete }`
  in `parse_slash`; a hidden-input mode built on the existing modal
  framework. Focus discipline follows #893 — keystrokes in the hidden field
  never reach the chat input or the approval gate.
- **Runtime** (`mur-agent-runtime`): `SecretVault` — name → `SecretString`,
  per process. A2A methods `secret/set` and `secret/delete`. Supervisor
  loads per-agent keychain entries into the vault before the sandbox seals.
- **Three injection points**: system-prompt fragment (names only, plus one
  line of git guidance: use `-H "Authorization: token $KEY"` or a credential
  helper, never put `$KEY` in a remote URL — advisory, not enforced);
  `bash.rs` spawn `.env(name, value)` per vault entry; tool-result
  chokepoint replacing every vault value with `[SECRET:<NAME>]`.

### Reused

`cmd_secret_set` / `cmd_secret_delete`, `keychain_get`, `SecretString`, the
`dial` unix-socket path, and the existing `secret list` / `doctor` /
`export` rules (secrets never travel with a bundle) — all of which cover the
new slot automatically because it is the same slot.

## Error and edge behavior

| Situation | Behavior |
|---|---|
| Keychain write fails | Whole command fails; runtime push is not attempted. Nothing "temporarily works" that would vanish on restart and look like a deletion. |
| Keychain ✓, runtime not running / socket down / `method not found` (older runtime) | `saved to keychain ✓ · running agent: not reached (restart to load)` — two lines, never collapsed into one ✓. `method not found` is the normal window after `mur update` when the CLI is newer than the runtime; it is "not reached", not a crash. |
| Both ✓ | `✓ GITEA_TOKEN available as $GITEA_TOKEN` |
| Invalid KEY or value < 8 chars | Rejected in the TUI before any write. |
| `--delete` | Keychain delete + `secret/delete` clears memory + system prompt refreshes. Same split reporting. |
| A keychain item this binary is not yet authorised for | macOS raises a modal prompt and the read blocks until it is clicked. The pre-seal read is therefore bounded (3 s, own thread): on timeout the agent logs a warning naming the secret and starts **without** it, rather than hanging before the sandbox seals. Found only on a real machine — CI has no keychain, and the provider keys never showed it because they were authorised long ago. |
| Value re-encoded (base64, split, hex-dumped) before being printed | Masking does not catch it. The spec states this ceiling explicitly: masking is defense in depth; the primary line is "plaintext never enters the context". |
| Masking mangles legitimate output that happens to contain the value | Accepted; the ≥ 8 floor makes this negligible. |

## Out of scope (each its own spec)

- MCP server env injection (`McpServerEntry` has no env field).
- Fleet-level sharing (revocation propagation, who may read from the signed channel).
- Agent-initiated secret requests (a natural-language "run `/secret GITEA_TOKEN`" is 90 % of it).
- Session-only volatile mode (`--session`); add when an OTP-style need appears.
- Enforcing the git rule by scanning `.git/config`.

## Verification

- **Unit:** `parse_slash("/secret X")`, `/secret X --delete`, KEY validation,
  length floor; `SecretVault` masking (multiple secrets, overlapping values,
  empty vault); system-prompt fragment carries names and no values.
- **Integration (runtime):** after `secret/set`, `echo $X` through the bash
  tool reaches the model as `[SECRET:X]`; `echo $X | base64` is **not**
  masked (negative control proving the documented ceiling); `method not
  found` from an older runtime yields "not reached", not a panic.
- **Real machine:** `murmur` against a running agent, `/secret`, then
  `grep -r <value> ~/.mur/agents/<agent>/channels/` returns zero hits;
  `mur agent secret list` shows the entry; after restart the agent has it
  without re-entry.

## Immediate operational note

Any token already pasted into a chat (as in the session that prompted this
design) is compromised by the definitions above — it is in a signed
append-only channel and at the provider. Revoke it at the issuer.

## Real-machine verification (2026-09-08)

Run against a throwaway agent (`secrettest`, `claude-haiku-4-5`), removed afterwards.

| Check | Result |
|---|---|
| `secret/set` on a live agent | `{"name":"GITEA_TOKEN","names":["GITEA_TOKEN"],"effective":"next-turn"}` — no value in the response |
| Runtime log line | `secret/set: vault updated name="GITEA_TOKEN"` — name only |
| Short value | rejected: `-32602 invalid params: secret 'SHORT' is shorter than 8 characters` |
| **The core guarantee** | one bash command returned `LEN=40 VAL=[SECRET:GITEA_TOKEN]` — the child process measured the real 40-character value while the model received the tag |
| Leak scan | zero hits for the value across all 26 agents' `~/.mur/agents/` trees (channels, telemetry, conversations) |
| Negative control | a planted canary file WAS found by the same grep, so the scan can detect what it claims to |
| `secret/delete` | `removed: true`, and the next turn reported `LEN=0` |
| Unreachable agent | dial fails with `agent 'secrettest' is not running (no running.lock)` — the string the `DurableOnly` reporting path renders |
| Keychain prompt | before the fix: agent hung in `__psynch_cvwait` with `SecurityAgent` up, never reaching ready. After: `keychain did not answer for this secret in time … timeout_secs=3` then `agent ready` |

Hits for the value in `~/.mur/queue/events.jsonl` and `~/.mur/session/recordings/` were attributed to the Claude Code session that typed it on its own command line (all 73 carry that session's id), not to any agent.

Not verified on a real machine: the TUI `/secret` command itself (hidden input needs an interactive terminal). Its parsing, validation, and reporting are unit-tested; the durable and dial halves it calls were exercised directly here.
