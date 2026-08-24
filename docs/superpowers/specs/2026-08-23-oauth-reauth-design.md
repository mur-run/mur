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

The message says "re-authenticate" but murmur has no `/login`, and it
names neither the credential nor where it lives. (`mur auth login` is
unrelated — that logs in to mur.run for the official catalog.)

## How the credential actually flows

```
agent/murmur → 127.0.0.1:8088 (mur-model-gateway)
                 ├─ Anthropic: keychain "Claude Code-credentials"  (60s memoise)
                 └─ Codex:     ~/.codex/auth.json
               → upstream provider
```

The gateway holds no credential of its own; it re-reads the CLI-owned
credential on every request. `~/.mur/secrets/anthropic.key` in the model
registry authenticates the *local* hop only — it is not what expires.

### The asymmetry (root cause)

`mur-model-gateway/src/lib.rs:579` retries a 401 for exactly one provider:

| | Anthropic (Claude Code) | Codex (ChatGPT) |
|---|---|---|
| Source | keychain `Claude Code-credentials` | `~/.codex/auth.json` |
| Blob carries `refresh_token` | yes | yes |
| **401 → refresh → retry** | **no** | yes, single-flight, one retry |
| Who refreshes | Claude Code itself | the gateway |

ChatGPT expiry is invisible to the user; Anthropic expiry is a hard 401.

### Two failure modes, previously conflated

| | State | Recovery |
|---|---|---|
| **A** | access token expired, refresh token valid | machine-recoverable, **no human, no browser** |
| **B** | not logged in; refresh token revoked or expired | needs a browser and a human |

Case A is the every-8-hours case and the one actually reported. Sending a
case-A user through a full browser OAuth flow is the wrong repair. The
design keeps them separate; the earlier draft did not.

### Measurements (2026-08-23)

- Keychain `mdat` 07:09:25Z vs `expiresAt` 15:09:25Z — Claude Code
  refreshes on an 8-hour cadence and writes back.
- `claude auth status` did not touch the keychain while the token was
  valid (`mdat` identical before and after) — safe as a probe.
- Both owner CLIs are installed and machine-readable: `claude auth status
  --json` (`loggedIn`, `email`, `subscriptionType` — **no expiry field**),
  `codex login status`. Normally present — they are what writes the
  credential the gateway reads — but not guaranteed: a transplanted
  credential (see Headless) leaves a working token with no owner CLI, so
  every path that shells out must degrade rather than assume.

## Principle: murmur never reads a secret

murmur needs to answer "is this credential healthy" and "did a refresh
just happen" without ever holding a token. Both are answerable from
non-secret sources:

- **Health** — the owner CLI reports it (`claude auth status --json`).
- **"A refresh happened"** — credential-store **metadata**: the keychain
  item's `mdat` (printed by `security find-generic-password` *without*
  `-w`, so no secret is read) and `~/.codex/auth.json`'s mtime.

Only the gateway parses the blob, because it necessarily holds the token
anyway. The `mur` repo gains no knowledge of credential formats.

## Design

Two independent halves; either ships alone and is useful.

### Half 1 — gateway: delegate the refresh, never perform it

The gateway is the chokepoint for *every* client, including unattended
agents, fleet runs and scheduled work where no human is watching. That is
why this half exists at all rather than living only in murmur.

On an Anthropic 401:

1. Parse `expiresAt` from the stored blob (a new field —
   `read_claude_code_oauth` returns only the token today).
2. **`expiresAt` still in the future** → the token was revoked, not
   expired. A probe cannot help. Return the actionable error immediately;
   do not spawn anything.
3. **`expiresAt` in the past** → case A. Under the existing single-flight
   discipline: run `claude auth status` (absolute resolved path, **stdin
   closed**, hard timeout from gateway config, output discarded), re-read
   the credential, retry the upstream request **once**.
4. **Negative cache** — if the probe did not move the store's `mdat`, do
   not re-probe for a cooldown window. Without this, an unrepairable state
   spawns a 325 MB process every 60 s (the cache TTL) forever.
5. Still failing → structured error naming the credential and the fix.

The gateway never reads or redeems the refresh token, so rotation cannot
race. That is the deliberate difference from the Codex path — see
Rejected. Codex behaviour is unchanged.

**Kill switch.** The probe lets a daemon mutate the user's Claude Code
credential without being asked. That gets a config flag, consistent with
this repo's practice for unattended automation. Default on — off by
default would mean the reported failure is never fixed for the users who
hit it hardest.

### Half 2 — murmur: `/login [provider]`

```
/login              status for every configured OAuth provider (read-only)
/login anthropic    escalating repair
/login chatgpt      escalating repair
```

Aliases: `anthropic`/`claude`, `chatgpt`/`codex`/`openai`. Bare `/login`
reports all providers, not just the running agent's — an agent's model can
change mid-session.

The name matches Claude Code's own `/login` for the same job. The help
text must distinguish it from `mur auth login` (mur.run), which is
unrelated.

**Escalating repair — cheapest rung that works:**

1. Re-read store metadata. The gateway's probe or Claude Code itself may
   have already fixed it.
2. **Cheap probe** — run the owner CLI's status command; compare `mdat`
   before/after. Moved ⇒ repaired. **No terminal handover, no browser.**
   This is the whole of case A.
3. **Full login** — case B only. Terminal handover to `claude auth login`
   / `codex login`.

Each rung reports which one succeeded, which also answers the open
empirical question below from field use.

**Single-flight.** `murmur a1 a2 a3` runs one process per pane; two
concurrent `/login` calls would launch two OAuth flows. A lock file under
`~/.mur/` serialises rung 3.

#### Terminal handover (rung 3 only)

murmur runs on the **main screen with a bottom-anchored
`Viewport::Inline`**, not the alternate screen (`TerminalGuard::enter`);
`sync_surface` enters the alt-screen only for heavy overlays. The child's
output therefore lands naturally in scrollback above the viewport — no
alternate screen is involved in the handover.

Constraints, all of them load-bearing:

- **Drop the `EventStream` first.** It owns stdin; a child inheriting
  stdin would race murmur for the user's keystrokes. The resize path
  already establishes this (`drop(events)` → rebuild → recreate).
- **Restore the full mode set**, not just raw mode: pop keyboard
  enhancement, `LeaveAlternateScreen` *iff* `ON_ALT`, disable bracketed
  paste and focus change, show cursor, disable raw mode — the exact
  sequence in `TerminalGuard::drop`.
- **Clear the viewport rows** before the child draws, or it paints over
  the remnants.
- **RAII re-entry.** Re-entering must survive the child panicking, exiting
  non-zero, or being killed — a guard whose `Drop` restores, not a
  happy-path return.
- **Re-anchor WITHOUT `ClearType::Purge`.** `purge_and_reanchor` purges
  scrollback, which would erase the login transcript — including the
  pasted-code prompt and any failure message. Handover needs a
  non-purging variant.
- **Keep the cursor-position retry loop.** `Terminal::with_options`
  queries the cursor; the just-dropped `EventStream`'s background thread
  holds crossterm's internal reader lock a beat longer.
- **Hazard:** `push/pop_keyboard_enhancement` and `ON_ALT` are
  process-global. Suspend/resume must balance against the session-lifetime
  `TerminalGuard` that is still alive.
- **SIGINT** during the child: ignore it in the parent for the duration so
  Ctrl-C reaches the child without killing murmur.

#### Headless

Before rung 3, check for a usable browser (`DISPLAY`/`WAYLAND_DISPLAY` on
Linux, always true on macOS, `SSH_CONNECTION` as a negative signal). This
is a heuristic, so `--print-only` forces the same output anywhere and
`--force-browser` overrides a false negative.

> **Deferred (2026-08-23): both override flags.** As shipped, `/login` takes
> a provider and nothing else — the heuristic decides, with no way to
> override it in either direction. `has_browser` is where they would attach
> and its doc comment says the same. They were dropped from the plan for
> want of evidence that the heuristic is wrong in practice; the first field
> report of a false negative (a viable browser this misses) or a false
> positive (a display that cannot actually open one) is what should bring
> them back. Nothing else in this section is deferred: the credential-
> injection and transplant paths below all ship.

With no browser, print the credential-injection path rather than launching
a flow that cannot complete:

- Anthropic: `claude setup-token` (long-lived token, requires subscription)
- Codex: `printenv OPENAI_API_KEY | codex login --with-api-key`, or
  `--with-access-token`

Also name the transplant path: log in where a browser exists and copy the
credential over. The gateway already reads a path
(`TokenSource::CredentialsFile` / `TokenSource::Codex`) and Linux installs
of Claude Code write `~/.claude/.credentials.json`.

### After a successful login

Nothing to restart — the gateway re-reads per request, so the next turn
picks up the new token. Say so explicitly; the natural assumption is that
the agent needs restarting.

## Error surface

Today the raw upstream body reaches the user and names no location. Every
new failure path names the credential, its store, and one command:

```
Anthropic OAuth expired (keychain "Claude Code-credentials", 2h ago)
  /login anthropic     — re-authenticate here
```

Revoked-not-expired gets its own wording, since re-running a refresh will
not help.

## Open empirical questions

1. **Does `claude auth status` refresh an expired token?** Unverified —
   it provably does nothing while the token is valid, which is expected
   and proves nothing about the expired case. The design degrades
   gracefully: the gateway logs once when a probe fails to move `mdat` and
   falls back to the actionable error; `/login` escalates to rung 3. Field
   data answers it.
2. **Does Anthropic's grant rotate the refresh token?** Refresh-token
   fingerprint at 2026-08-23 15:51 local was `bd5d5ff4354f`;
   re-fingerprint after the next Claude Code refresh (`expiresAt` 23:09
   local). Changed = rotates. Recorded because it would decide a *different*
   design; it does not gate this one.

## Testing

- Gateway: expired-vs-revoked branch — a 401 with `expiresAt` in the
  future must not spawn anything.
- Gateway: 401 → probe → retry with the probe stubbed. Exactly one retry;
  a second 401 propagates unchanged; refresh token never read.
- Gateway: negative cache — a probe that fails to move `mdat` is not
  retried within the cooldown.
- Gateway: no `claude` on PATH → actionable error, no panic, no hang.
- Gateway: kill switch off → no spawn.
- murmur: `parse_slash` for `/login`, each alias, unknown provider.
- murmur: status rendering from captured `claude auth status --json` and
  `codex login status` fixtures — logged in, logged out, malformed.
- murmur: rung 2 mtime detection — moved and not-moved.
- murmur: headless matrix over the env-var combinations.
- Manual: terminal restored after the child exits 0, exits non-zero, and
  is SIGKILLed mid-flow; scrollback still holds the login transcript
  afterwards.

## Rejected

**Gateway redeems the refresh token itself (mirroring Codex).** Needs a
second gitignored OAuth-constants module and a new `has_anthropic_hook`
cfg, and whether Anthropic's grant rotates the refresh token is
unverified. If it rotates, the gateway and Claude Code race to redeem the
same single-use credential and the loser is stranded — Codex must persist
rotation for exactly this reason (`src/codex.rs`: discarding the new pair
"strands both this gateway and Codex CLI on a dead credential").
Delegating makes the question moot.

**Putting the delegated probe only in murmur.** Cleaner (a CLI spawning a
child is unremarkable; a daemon doing it is not), but it would leave
unattended agents, fleets and scheduled runs — the paths with nobody
watching — hard-failing every 8 hours. Coverage wins; the daemon's spawn
is fenced by an absolute path, closed stdin, a timeout, a negative cache
and a kill switch.

**murmur implements OAuth directly** (browser, PKCE, callback server).
Duplicates client constants and every headless edge the owner CLIs have
already solved, and risks writing the credential store in a shape those
CLIs disagree with.

**murmur reads the credential blob to display expiry.** Store metadata
plus the owner CLI's own status answers the same questions and keeps the
`mur` repo free of credential-format knowledge and of secret handling.

**A gateway status endpoint for murmur to query.** The router is a
catch-all proxy (`/` + `/{*tail}`); a reserved prefix would shadow
upstream paths, and it would make `/login` status depend on the gateway
being up. Metadata gets the same answer with no new surface.

**murmur parsing the child's stdout to keep the TUI in control** (render
the OAuth URL in-TUI, pipe the pasted code to the child's stdin). Avoids
handover entirely but depends on Claude Code's output format and on
whether it uses a local callback server instead of paste-code. Too
brittle.

**Cache invalidation alone.** Covers only the ≤60 s window where Claude
Code already refreshed. The reported failure was Claude Code not running
at all; re-reading returns the same dead token.

**Spawning a terminal window for the login flow.** Fails under SSH and
headless — exactly where re-auth is hardest — so the print-only path is
needed anyway. One path, not two.

**Auto-running `/login` on a 401.** Seizing the terminal in response to an
upstream error is too surprising. Half 1 already covers case A silently;
what remains needs a human.

**Deferred, not rejected: proactive refresh.** Probing when `expiresAt` is
within a skew window would avoid the failed request entirely, but it moves
the spawn onto the normal request path. Revisit once the negative cache
and the rung-2 field data exist.

## Where this lives

- `mur-model-gateway/src/lib.rs` — 401 retry eligibility, Codex-only
  (~line 579); router is proxy-only (~line 314)
- `mur-model-gateway/src/keychain.rs` — `read_claude_code_oauth` returns
  only the token; `expiresAt` parsing is new. `CACHE_TTL` lives here.
- `mur-core/src/cmd/agent/cli/app.rs` — `SlashCmd`, `parse_slash`
- `mur-core/src/cmd/agent/cli/mod.rs` — slash dispatch (~1826);
  `TerminalGuard` (~302); `sync_surface`/`ON_ALT` (~501);
  `purge_and_reanchor` (~610); the resize path's `drop(events)` (~669)
