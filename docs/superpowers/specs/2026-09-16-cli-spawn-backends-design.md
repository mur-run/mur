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

Rows marked **run** were produced by executing the CLI and reading what came
back; the rest by scraping its own help. Nothing here is inferred. Measured
2026-09-16.

| | `claude` 2.1.273 | `codex` 0.154.0 | `agy` 1.2.3 |
|---|---|---|---|
| headless | `-p` / `--print` | `exec` (prompt on **stdin**) | `-p` / `--print` |
| streaming flag | `--output-format stream-json` | `--json` | `--output-format stream-json` |
| incremental deltas (**run**) | yes | **no** — completed items only | yes — `text_delta` |
| envelope key (**run**) | `type` | `type` | `event` |
| disable built-in tools (**run**) | **yes** — `--tools ""` | **no** — shell is core; only `-s read-only` | **no** — 57 built-ins, no disable flag |
| mount host tools (MCP) | **per-call** — `--mcp-config`, `--strict-mcp-config` | persistent — config `mcp_servers` | persistent — `agy mcp add` only |
| private home | `CLAUDE_CONFIG_DIR` | `CODEX_HOME` | **none** — only `HOME` |
| system prompt | `--append-system-prompt` | unverified | not exposed |

`agy` reports **zero** per-call MCP/tool flags. That single fact drives the
isolation decision below.

### `--disallowedTools` is not the tool-disable flag

The earlier draft named it. It is a *named deny list* — "Comma or space-
separated list of tool names to deny". Denying `Bash` and asking for a
directory listing got `WORKED Glob`: the model simply reached for another
built-in. Enumerating every built-in to deny would be a list that rots on
each release.

`--tools ""` is the real mechanism ("Use `""` to disable all tools"). Asked
to run a shell command with it set, `claude` answered `NO_TOOLS`; the same
prompt without it ran the command.

### Disabling built-ins does not empty the tool list

`--tools ""` alone left **44 tools** mounted — every MCP server in the user's
own `~/.claude` config (chrome-devtools, a database client, others). Read off
the `system init` event's `tools` array, so this is what the model was
actually offered, not what it said about itself.

That is the hazard the private home exists for, stated concretely: spawning
`claude` against the user's own config would hand the model dozens of tools
MUR never authorized, none of them passing MUR's handler, its entitlements or
its HITL gate.

The verified recipe is all three flags together:

```
claude -p --tools "" --strict-mcp-config --mcp-config <mur's own>
```

`system init` then reports `tools: []` — exactly empty, before MUR mounts its
own. `--strict-mcp-config` is load-bearing, not decoration.

### What `agy` tells us about itself

Its `init` event lists **57 built-in tools**, including `run_command`,
`write_to_file`, `send_command_input`, `execute_browser_javascript` and a full
browser-control set. So the activation gate does not have to guess at agy's
capabilities — the CLI enumerates them every turn. It also has no equivalent
of `--tools ""`, which is why no exception is accepted for it below.

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

So a private home under `~/.mur/cli-homes/<backend>/` remains available, and
for `agy` it is the only option. For `claude` it is **not the default**, and
the three sentences below replace the one this section used to carry ("the
user's own CLI configuration is never read or written"), which the default
contradicts. Each is stated so it can be checked rather than believed.

**Read.** With `CLAUDE_CONFIG_DIR` unset, a spawn reads the user's own login
from `~/.claude`. Deliberate, not a gap: it is what removes the second login.
Measured — a private `CLAUDE_CONFIG_DIR` pointed at an empty directory
answers `Not logged in · Please run /login`, so the credential does not come
from the Keychain alone and a private home costs a real re-authentication.
*Checkable:* `claude_home()` returns `None`, and the spawn sets no
`CLAUDE_CONFIG_DIR`.

**Write.** Each `claude` turn writes into the user's `~/.claude`: a session
transcript at `projects/<escaped-cwd>/<uuid>.jsonl`, an entry under
`session-env/<uuid>`, and a touch of `plugins/cache/.../.in_use`. Known and
accepted. The transcript is keyed by the **spawn cwd**, not by MUR — measured
by spawning from two directories and getting two `projects/` entries named
after each — so an agent working inside one of the user's repositories puts
its turns into that repository's `claude --resume` history, interleaved with
the user's own. Agents need the project's files, so the cwd is not free to
move; this is a cost to know, not one to design around yet.

The `.in_use` marker is different in kind and should not be read as history:
it is a concurrency lock, so a user's own `claude` session running at the
same time as a MUR turn contends on the same marker.

*Checkable:* there is no flag or environment variable that disables session
persistence. Verified against `claude` 2.1.274 by scanning every
`CLAUDE_CODE_DISABLE_*` string in the binary and filtering for
session/transcript/history/persist/log/project/save/write — one unrelated
hit — and by reading `--help`. If a future version adds one, the Write
paragraph is what changes.

**Isolation.** Tool isolation does not come from the home. It comes from
`--tools "" --strict-mcp-config --mcp-config <ours>`, which reports
`tools: []` before MUR mounts its own — including against the user's own
home. A private home is an optional override with exactly one entry point,
`claude_home()`, so switching this decision is one implementation rather
than a condition spread across the spawn path. *Checkable:* the spawn's
`system init` lists MUR's tools and nothing else.

The lever differs per backend, and `agy`'s is blunt: `CODEX_HOME` and
`CLAUDE_CONFIG_DIR` relocate one CLI's config, but agy has no such variable,
so only `HOME` works (probe 1). Redirecting `HOME` moves everything that
process resolves under it, which is more isolation than intended and has to
be handled deliberately rather than inherited by accident.

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

`--mode plan` is not a verified tool-disable or security boundary, and
`--sandbox` has now been measured and is not one either: with the approval
layer disabled it still read outside the workspace, read from `$HOME`, wrote
outside the workspace and reached the public internet (probe 6). Unlike the
conditional codex exception, no exception is accepted for agy's built-in
tools — of which its own `init` event enumerates 57, including `run_command`
and `write_to_file`.
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

## Answered by probe (2026-09-16)

1. **`agy`'s home environment variable: there is none.** No `AGY_HOME`, no
   `ANTIGRAVITY_HOME`; `XDG_CONFIG_HOME` is read by the binary but does not
   relocate the config. Only `HOME` does. Its MCP config lives at
   `$HOME/.gemini/config/mcp_config.json` — a Gemini CLI inheritance, not
   under `.antigravity` at all.

   This is a coarser lever than `CODEX_HOME` / `CLAUDE_CONFIG_DIR` and the
   design must say so: relocating `HOME` moves *everything* that process
   resolves under it — login, caches, anything else agy reads — not just the
   MCP config.

2. **Streaming envelopes: both captured**, shapes in the capability table
   above. The finding that matters is that `codex exec --json` emits no
   incremental text: a 40-line reply still arrived as exactly four events,
   the whole body inside one `item.completed`. A codex-backed turn therefore
   cannot stream partial output to a MUR channel. `agy` can, via
   `step_update.text_delta`.

3. **`agy mcp add` can target a non-default home, via `HOME` only.** It has
   no scope or config-path flag. Verified both ways: with `HOME` redirected
   the entry landed in the temp home and the user's real config was
   untouched; with `XDG_CONFIG_HOME` redirected it was written to the user's
   real config instead.

5. **`--disallowedTools` is a named deny list, not a tool-disable.** See the
   capability table. `--tools ""` is the mechanism, and it is only sufficient
   alongside `--strict-mcp-config`.

6. **`agy --sandbox` restricts none of the four requirements.** Probed with
   `--dangerously-skip-permissions` so the approval layer was out of the way
   and the sandbox was the only thing that could refuse:

   | attempted with `--sandbox` | result |
   |---|---|
   | read a file outside the workspace | contents returned |
   | read `$HOME/.gemini/config/mcp_config.json` | contents returned |
   | write a file outside the workspace | written — verified on disk |
   | `curl https://example.com` | `200` |

   The binary does contain Seatbelt machinery (`sandbox-exec`,
   `sandbox_mounts`, `sandbox_allow_network`, `sandbox_system_allowlist`), so
   a policy engine exists. What the probe shows is that the documented flag,
   invoked the way its help describes, engages nothing that meets the
   activation gate's filesystem, secret-access or network requirements.

   Scope of the claim: this is the bare `--sandbox` flag. Those same strings
   hint at configuration (`sandbox_mode`, `sandbox_override`) that may be
   settable by some other route. "Not a boundary as offered" is the finding;
   "agy cannot be sandboxed" is not.

## Open questions

4. Whether `claude` should keep a private home at all. The probe strengthens
   the alternative: `--tools "" --strict-mcp-config --mcp-config <file>`
   yields an empty tool list *even against the user's own home*, so tool
   isolation does not require one. What a private home still buys is a
   separate credential; what it costs is the second login the Goal prices.
   The trade is now fully informed and is a decision, not a probe.

## Verification plan

- Tool-list emptiness, per spawn, asserted from the CLI's own report rather
  than the model's: `claude`'s `system init` event must show `tools: []`
  before MUR mounts its own. A behavioural prompt ("do you have a tool?") is
  not evidence — the model answered `NO_TOOLS` in a run where 44 ambient MCP
  tools were in fact mounted.
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
