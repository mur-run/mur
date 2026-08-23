# OAuth Re-Authentication: `/login` and Gateway Delegated Refresh

Status: design (2026-08-23). Two repos: `mur` (murmur TUI) and
`mur-model-gateway`.

## Problem

An agent turn dies with an opaque 401 and no way to recover from inside
the TUI:

```
error: auth refused (401): {"type":"error","error":{"type":"authentication_error",
"message":"OAuth access token has expired. Re-authenticate to continue."}}
```

The message says "re-authenticate" but murmur has no `/login`, and the
message never says *which* credential expired or *where* it lives. The
user is left guessing. (`mur auth login` is unrelated — that logs in to
mur.run for the official catalog.)

## How the credential actually flows

```
agent/murmur → 127.0.0.1:8088 (mur-model-gateway)
                 ├─ Anthropic: keychain "Claude Code-credentials"  (60s memoise)
                 └─ Codex:     ~/.codex/auth.json
               → upstream provider
```

The gateway holds no credential of its own. It re-reads the CLI-owned
credential on every request. `~/.mur/secrets/anthropic.key` in the model
registry authenticates the *local* hop only; it is not what expires.

### The asymmetry (root cause)

`mur-model-gateway/src/lib.rs:579` retries a 401 for exactly one provider:

| | Anthropic (Claude Code) | Codex (ChatGPT) |
|---|---|---|
| Source | keychain `Claude Code-credentials` | `~/.codex/auth.json` |
| Blob carries `refresh_token` | yes | yes |
| **401 → refresh → retry** | **no** | yes, single-flight, one retry |
| Who refreshes | Claude Code itself | the gateway |

ChatGPT expiry is invisible to the user. Anthropic expiry is a hard 401.

### Measurements (2026-08-23)

- Keychain `mdat` 07:09:25Z vs `expiresAt` 15:09:25Z — Claude Code
  refreshes on an 8-hour cadence and writes back.
- `claude auth status` did not touch the keychain while the token was
  valid (`mdat` identical before and after) — safe to use as a probe.
- Both owner CLIs are installed and expose machine-readable surfaces:
  `claude auth status --json` (`loggedIn`, `email`, `subscriptionType`),
  `codex login status`. They must be installed — they are what writes the
  credential the gateway reads.

## Design

Two independent halves. Either ships alone and is useful.

### Half 1 — gateway: delegate the refresh, never perform it

On an Anthropic 401, the gateway escalates without ever touching the
refresh token:

1. Invalidate the 60s keychain cache and re-read. Claude Code may have
   refreshed inside the cache window.
2. Still rejected → run `claude auth status` (timeout from gateway config,
   not a literal; output discarded). This hands the refresh to the process that owns the
   credential.
3. Re-read the keychain, retry the upstream request **once**.
4. Still failing → return an actionable error naming the credential and
   the fix, instead of the raw upstream body.

Only one process ever redeems the refresh token, so rotation cannot race.
This is the deliberate difference from the Codex path — see Rejected.

Codex behaviour is unchanged.

### Half 2 — murmur: `/login [provider]`

```
/login              status for every configured OAuth provider
/login anthropic    status; if not logged in, hand over to `claude auth login`
/login chatgpt      status; if not logged in, hand over to `codex login`
```

Bare `/login` reports all providers, not just the running agent's — an
agent's model can change mid-session, and seeing both is the point.

Provider aliases: `anthropic`/`claude` and `chatgpt`/`codex`/`openai`.

Status is read-only and never mutates a credential.

#### Terminal handover

`claude auth login` is interactive and murmur owns the terminal. `/login
<provider>` suspends the TUI the way an editor handover works: leave raw
mode and the alternate screen, restore the cursor, run the child inheriting
all three stdio handles, then re-enter and force a redraw.

Restoration must survive the child panicking, being killed, or exiting
non-zero — the terminal is restored in a guard that runs on every exit
path, not after a happy-path return.

#### Headless

Before handing over, check for a usable browser (`DISPLAY`/`WAYLAND_DISPLAY`
on Linux, always true on macOS, plus `SSH_CONNECTION` as a negative
signal). With no browser, print the credential-injection path instead of
launching a flow that cannot complete:

- Anthropic: `claude setup-token` (long-lived token, requires subscription)
- Codex: `printenv OPENAI_API_KEY | codex login --with-api-key`, or
  `--with-access-token`

Also name the transplant path: log in on a machine that has a browser and
copy the credential over. The gateway already reads a path —
`TokenSource::CredentialsFile` / `TokenSource::Codex` — and Linux installs
of Claude Code write `~/.claude/.credentials.json`.

`--print-only` forces this output on any platform.

### After a successful login

Nothing to restart. The gateway re-reads the credential per request, so
the next turn picks up the new token. Say so explicitly — the natural
assumption is that the agent needs a restart.

## Error surface

The 401 body is what the user sees today, and it names no location. Every
new failure path names the credential, its store, and one command:

```
Anthropic OAuth expired (keychain "Claude Code-credentials", 2h ago)
  /login anthropic     — re-authenticate here
```

## Testing

- Gateway: 401 → probe → retry, with the probe stubbed. Assert exactly one
  retry, that a second 401 propagates unchanged, and that the refresh token
  is never read by gateway code.
- Gateway: no `claude` binary on PATH → actionable error, no panic, no hang.
- murmur: `parse_slash` cases for `/login`, each alias, and an unknown
  provider.
- murmur: status rendering from captured `claude auth status --json` and
  `codex login status` fixtures — logged in, logged out, malformed.
- murmur: headless detection matrix over the env-var combinations.
- Manual: terminal restored after the child exits 0, non-zero, and is
  SIGKILLed mid-flow.

## Rejected

**Gateway redeems the refresh token itself (mirroring Codex).** Needs a
second gitignored OAuth-constants module and a new `has_anthropic_hook`
cfg, and — decisively — whether Anthropic's grant rotates the refresh
token is unverified. If it rotates, the gateway and Claude Code race to
redeem the same single-use credential and the loser is stranded. Codex
must persist rotation for exactly this reason (`src/codex.rs`: discarding
the new pair "strands both this gateway and Codex CLI on a dead
credential"). Delegating makes the question moot.

An open experiment can settle it for the record: refresh-token
fingerprint at 2026-08-23 15:51 local was `bd5d5ff4354f`; re-fingerprint
after the next Claude Code refresh (`expiresAt` 23:09 local). Changed =
rotates. This does not gate the design.

**murmur implements OAuth directly** (browser, PKCE, callback server).
Duplicates client constants and every headless edge the owner CLIs have
already solved, and risks writing the credential store in a shape those
CLIs disagree with.

**Cache invalidation alone.** Only covers the ≤60s window where Claude
Code has already refreshed. The reported failure was Claude Code not
running at all; re-reading returns the same dead token.

**Spawning a terminal window for the login flow.** Fails under SSH and
headless — exactly where re-auth is hardest — so the print-only path is
needed regardless. One path, not two.

**Auto-running `/login` on a 401.** Seizing the terminal in response to an
upstream error is too surprising. The gateway's delegated refresh already
covers the recoverable case silently; what remains needs a human.

## Where this lives

- `mur-model-gateway/src/lib.rs` — 401 retry eligibility, currently
  Codex-only (~line 579)
- `mur-model-gateway/src/keychain.rs` — cache TTL, credential read
- `mur-core/src/cmd/agent/cli/app.rs` — `SlashCmd`, `parse_slash`
- `mur-core/src/cmd/agent/cli/mod.rs` — slash dispatch (~line 1826)
