# CLI-spawn backends — a second track for subscription models

Status: **draft, awaiting approval**
Date: 2026-09-16

## Problem

A ChatGPT subscription reaches MUR through exactly one path today: the
loopback `mur-model-gateway` compiled with the gitignored
`src/codex/codex_impl.rs`. That file has never been committed and CI's
`release.yml` is a plain `checkout` + `cargo build`, so **every published
gateway is a stub** and `/__mur/health` answers `codexHook: false`. The Hub
then offers a ChatGPT provider that cannot work, and its "repair" button
reinstalls the same stub — it succeeds and changes nothing, because a
compile-time `cfg` is not something reinstalling can flip.

The same shape applies to Claude (`src/disguise/disguise_impl.rs`,
`has_beta_hook`), and Gemini/Antigravity has no hook at all, so it has no
path whatsoever.

## Goal

A second, independent track that reuses the user's **already-installed
coding CLI**, not its existing login session. Each backend requires a separate
subscription login in MUR's private CLI home (see "Isolation" below). MUR keeps
its own skills, MCP tools, entitlements and approval gate.

### What this buys, given the login is not free

The reuse is of the **binary, not the credential**, so the obvious question is
why MUR does not simply hold the subscription token itself. That is the gateway
track, and it is gated on the gitignored `codex_impl.rs` / `disguise_impl.rs`
which no published build has and which this design does not reconstruct (see
"Non-goals"). The coding CLI is the vendor's own client for those endpoints: it
already ships, and when the vendor changes the protocol it is the vendor who
updates it. What the extra login buys is not having to build and maintain a
second implementation of that transport.

### Hard constraint

**The existing gateway-hook track is unchanged.** A build that has
`codex_impl.rs` or `disguise_impl.rs` behaves exactly as it does today: same
route, same `provider: codex` / `provider: claude` clients, same loopback
validation, same `/__mur/health` fields. This design only *adds* a track and
a selector. Nothing in `mur-model-gateway`, `llm/codex.rs`, `llm/claude.rs`
or `llm/loopback.rs` changes behaviour. See "Non-goals".

## Measured capabilities

All verified on this machine, 2026-09-16, by scraping each CLI's own help.
Nothing here is inferred.

| | `claude` 2.1.272 | `codex` 0.154.0 | `agy` 1.2.3 |
|---|---|---|---|
| headless | `-p` / `--print` | `exec` | `-p` / `--print` |
| streaming | `--output-format stream-json` | unverified | `--output-format stream-json` |
| disable built-in tools | **yes** — `--disallowedTools` (scope: Q5) | **no** — shell is core; only `-s read-only` | **no** — only `--mode plan` |
| mount host tools (MCP) | **per-call** — `--mcp-config`, `--strict-mcp-config` | persistent — config `mcp_servers` | persistent — `agy mcp add` only |
| system prompt | `--append-system-prompt` | unverified | not exposed |

`agy` reports **zero** per-call MCP/tool flags. That single fact drives the
isolation decision below.

## Rejected: "CLI as a model"

Spawn the CLI with every built-in tool disabled and use it as a plain
completion endpoint, MUR keeping its loop.

This does not work, and the reason is structural rather than a missing flag.
The model runs **inside the CLI's process**; the tool list it sees is the
CLI's. With the CLI's tools off and MUR's tools not mounted, the model can
never emit a call for a MUR tool — it does not know they exist. MUR receives
prose, never a `tool_call`, so nothing drives skills, MCP or bash. The agent
degrades to chat-only, which discards exactly the capabilities this design
exists to preserve.

## Chosen: CLI owns the loop, MUR owns the tools

The CLI runs its own agentic loop. MUR's tools are mounted **into** it as an
MCP server that MUR itself serves. The model sees MUR's tools; when it calls
one, execution happens in MUR's handler — which is where entitlements,
secret masking and the HITL gate already live. These guarantees apply only
to calls through MUR's handler; built-in CLI tools do not inherit them and
must satisfy the isolation and activation requirements below.

```
CLI process (loop, model, subscription creds)
        │  MCP (stdio)
        ▼
MUR MCP handler  ──►  entitlements · secret masking · HITL gate
        │
        ▼
   skills · tools · bash
```

## Isolation: a private CLI home per backend

Two facts force this:

1. `codex` and `agy` can only take MCP config **persistently**. Writing
   MUR's tools into the user's own config would silently change the CLI they
   use interactively — an uninvited side effect.
2. Credentials cannot be shared by copying. `~/.codex/auth.json` and
   `~/.claude` + Keychain hold rotating OAuth refresh tokens; two holders
   each refreshing will invalidate the other's token and **log the user out
   of their own CLI**. This is precisely why `mur-model-gateway` is the sole
   token holder today.

So each spawn backend gets its own home (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`,
`agy`'s equivalent — to be confirmed) under `~/.mur/cli-homes/<backend>/`.
MUR's MCP config is written **once into that home**. The user's own CLI
configuration is never read or written.

Accepted cost, confirmed by the user: **one extra login per backend.** The
private home starts empty, so the user authenticates once inside it. We do
not copy `auth.json` to avoid this — see (2).

**A separate login is not a second holder.** (2) describes copying a
credential: both copies then refresh the *same* refresh-token lineage, and
whichever refreshes second is handed a token the first has already rotated
away. An independent login inside the private home mints its own lineage, so
the two sit side by side. That distinction is the entire reason "log in again"
is a fix rather than the same bug spelled differently — and it is an assumption
about each vendor's token service, not something MUR controls. It is therefore
the first thing the verification plan proves, per vendor, before that backend
ships.

`claude` does not strictly need a private home (`--mcp-config` +
`--disallowedTools` are per-call), but uses one anyway so all three backends
share a single lifecycle and one mental model. Note what that now costs: a
second `claude login`, charged purely for uniformity, on the one backend that
could have run against the user's existing session without writing to it.
Uniformity was cheap while the cost was invisible; the Goal has stopped it
being invisible. Open question 4 records the alternative rather than silently
keeping a decision that predates the correction.

## Conditional boundary: codex keeps an unmediated shell

`codex` cannot remove its built-in shell. `-s read-only` restricts filesystem
writes; it does not establish action safety. Shell commands can still execute,
read secrets, or cause network side effects without passing MUR's HITL gate.
A private CLI home separates configuration and credentials, not process access.

Before enabling this backend, enforce and verify a process sandbox inherited
by child processes: filesystem access is limited to explicitly approved inputs
and the private runtime home, unrelated secrets and inherited credentials are
inaccessible, and writes are limited to required private runtime state. Deny
network access by default, allowing only the documented authentication/model
endpoints and MUR tool transport needed by the backend. These exceptions and
any residual unmediated actions must be documented and explicitly accepted;
labels alone are not enforcement. If these restrictions cannot be enforced or
verified, CLI spawn remains disabled rather than falling back to weaker flags.

The agent's status and Hub panel must disclose that built-in shell execution
is unmediated, identify the verified sandbox boundaries, and avoid claiming
that read-only mode prevents HITL bypass. MUR's gate still protects calls
through its own handler, not arbitrary built-in CLI actions.

## Activation gate: agy remains disabled

`--mode plan` is not a verified tool-disable or security boundary. Unlike the
conditional codex exception, no exception is accepted for agy's built-in tools.
Keep agy CLI spawn disabled until probes verify private-home selection, MCP
configuration isolation, built-in tool capabilities, and effective filesystem,
secret-access and network restrictions (including child processes). Every
built-in action must either be disabled or have its unmediated scope documented
and explicitly approved under a backend-specific governance boundary. Failure
or unknown results keep the backend disabled; binary presence is insufficient.

## Backend registry, not three hardcoded paths

Gemini CLI was replaced by Antigravity inside a year. Hardcoding per-CLI
flags guarantees rewriting this on the next replacement. Each backend is a
data record:

```
binary, headless_invocation, stream_flags, tool_disable_flags,
mcp_mount (per-call | persistent), home_env_var, capability_notes
```

Backends whose binary is absent do not appear in the UI at all.

## Track selection and the Hub button

A vendor now has up to two tracks. Availability is per-track:

| gateway hook | CLI present | CLI safety gate | offered | action |
|---|---|---|---|---|
| true | any | — | gateway (preferred) | normal |
| unknown | no | — | gateway | CTA → install / start gateway |
| unknown | yes | passed | gateway CTA, CLI spawn below it | resolve the unknown before spending a login; CLI spawn available, not preselected |
| unknown | yes | failed / unverified | gateway | CTA → install / start gateway; CLI spawn shown disabled with its unmet safety requirements |
| false | yes | passed | CLI spawn | normal |
| false | yes | failed / unverified | neither | both disabled; names the unmet safety requirements |
| false | no | — | neither | disabled, says which track is missing |

`unknown` never disables a control and never routes silently to CLI spawn.
That fallback costs a second login and, for codex, an unmediated shell — far
too much to spend on a question we merely failed to ask, when the answer may
well be that the gateway works. Resolve it first.

This supersedes hiding the provider. Hiding punished users who had already
registered models — the wrong target. The control that must reflect
availability is the **button**, not the provider's visibility. `false` means
"this build denied it"; `unknown` means "could not ask" and must never be
folded into `false` — including in this table, which is why no row groups them.

## Non-goals

- Changing the gateway track in any way.
- Making ChatGPT work on a stub gateway *through the gateway*. The stub
  stays a stub; the CLI track is the answer for users without the hook.
- Committing `codex_impl.rs` / `disguise_impl.rs`, or reconstructing them.
- Mediating codex's built-in shell through MUR's HITL gate (enforcing and
  verifying its process sandbox remains a requirement).

## Open questions

1. `agy`'s home environment variable — not yet identified; `CODEX_HOME` and
   `CLAUDE_CONFIG_DIR` are known.
2. `codex exec` and `agy` streaming envelope shapes — `claude`'s
   `stream-json` is confirmed; the other two need a probe.
3. Whether `agy mcp add` can target a non-default home purely via env.
4. Whether `claude` should keep a private home at all, now that the Goal
   prices it. The alternative is per-call `--mcp-config --strict-mcp-config
   --disallowedTools` against the user's own home: it writes nothing and
   leaves the CLI as sole token holder, so (2) does not apply and the second
   login disappears. Cost of the alternative: one backend whose lifecycle
   differs from the other two.
5. What `--disallowedTools` actually disables. The capability table's **yes**
   rests on the flag appearing in `claude --help`, which does not establish
   that it can disable *every* built-in rather than a named list. Probe before
   relying on it as the tool-isolation mechanism.

## Verification plan

- Token lineage, per vendor, gating that backend's ship: log in inside the
  private home, force a refresh there, then assert the user's own CLI is still
  authenticated — and the reverse. The isolation check below is not sufficient
  evidence for this: invalidation happens at the vendor, so the user's config
  files can sit untouched while their session is already dead.
- Backend registry: table-driven unit tests, one row per backend.
- Isolation: assert the user's `~/.codex`, `~/.claude` and agy config are
  untouched after a spawned turn (mtime + content).
- Governance: a spawned turn calling a `write`-risk MUR tool must pause on
  the HITL gate exactly as an in-process turn does.
- Built-in tool isolation: attempt reads of unrelated secret fixtures, writes
  outside private runtime state, and requests to a non-allowlisted local test
  endpoint, both directly and via child processes; assert sandbox denial.
  Verify documented authentication/model and MCP paths still work without
  exposing real credentials in test logs.
- Activation: codex and agy remain disabled when any required safety probe is
  missing or fails; installing the binary alone must not enable spawn. Assert
  agy's private-home/MCP isolation and approved governance boundary are required,
  and that status/Hub disclose unmediated tools without claiming HITL coverage.
- Regression: with a hook-enabled gateway, every existing codex/claude path
  behaves identically — the hard constraint, asserted not assumed.
