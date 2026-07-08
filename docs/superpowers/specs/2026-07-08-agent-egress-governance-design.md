# Agent Egress Governance + CLI Hardening (Design)

Date: 2026-07-08
Status: Design — implementation scoped to Phase 1 (+ CLI hardening); Phases 2–4 are roadmap.

## 1. Context

Building the AURA web-research agent surfaced one load-bearing gap and a batch of CLI
defects. The gap — an agent that legitimately needs broad web egress has no safe way to
get it — is not a one-off; it is the seed of MUR's long-term, enterprise-grade **agent
egress governance**. This spec designs that governance model in full but scopes the
*implementation* to Phase 1, and folds in the mechanical CLI fixes as a separate part.

The enterprise concern is not "can an agent reach the internet" but **"can sensitive
data leave, to where, and can we prove/stop it."** A static host allowlist does not
answer that (data can exfiltrate to an allowed host). So egress governance must be
least-privilege, centrally policyable, fully audited, instantly revocable, and mappable
to compliance (SOC2/ISO).

## 2. Part A — Agent Egress Governance

### 2.1 Principle: separate policy, enforcement, and audit

Three planes, deliberately decoupled so each can evolve independently:

1. **Policy plane (control).** Declarative egress policy bound to `(agent, tool)`,
   versioned and Ed25519-signed, distributable via Commander / signed channels (central
   governance in enterprise; a local file for solo users). Answers "who / which tool /
   may go where, at what tier."
2. **Enforcement plane (data path).** Two enforcers:
   - **sbpl kernel** — coarse: `deny` / `allow-to-host-list` / `allow-to-proxy`. Exists today.
   - **managed egress proxy** — fine: per-request allow/deny, DLP inspection, rate limit.
     Later phase; sbpl pins egress so it can *only* reach the proxy (un-bypassable).
3. **Audit plane (evidence).** Every grant and every egress event appends to an
   immutable log tied to the existing `GovernanceState` and channel signing.

### 2.2 Mode ladder (least-privilege, escalating authority)

```
off  →  allowlist(static hosts)  →  broad-audited  →  unrestricted
```

- Each higher tier requires higher authority to enable: `self < operator-consent <
  Commander-signed policy`.
- **`broad-audited` (Phase 1, the research-agent unblock):** permits all outbound, BUT
  (1) overlays the existing deny-host list, (2) requires explicit operator opt-in,
  (3) renders a loud `BROAD EGRESS ON` line in `mur agent perm show`, (4) emits a
  telemetry/`GovernanceState` event on enable and (Phase 2) per egress.
- `unrestricted` stays as the un-audited escape hatch (unchanged), but is now the top of
  a labeled ladder rather than the only alternative to `restricted`.

### 2.3 Cross-cutting dimensions

- **Scoping (defense in depth).** Policy binds to `(agent, tool)`: "agent-browser may
  reach the web; the rest of the agent may not." Enforced fully in Phase 4; the policy
  schema carries the `tool` binding from Phase 1 so nothing is re-modeled later.
- **Commander integration.** Egress becomes a governed capability alongside `kill` and
  `budget-ceiling`. A Commander directive can revoke or cap egress network-wide (an
  exfiltration kill-switch). Governance errors are **fail-closed** (matches the existing
  loop/daemon pattern).
- **Portability.** Exported `.muragent` bundles carry egress policy, but **import always
  re-approves locally and never auto-grants `broad-audited`+** (matches the fleet-import
  "install at lowest trust" rule).

### 2.4 Data model (Phase 1 lands the full shape)

Extend `entitlements.network.outbound` in `mur-common/src/agent.rs`:

- `NetworkOutboundMode` gains `BroadAudited` (between `Restricted` and `Unrestricted`).
- `outbound` gains: `tool_scope: Option<String>` (the MCP server name egress is bound to;
  `None` = whole agent), and `deny_hosts: Vec<String>` (overlay; already partly present).
- `GovernanceState` gains an `egress` section: current mode, who authorized it, when,
  and (Phase 2) an append-only egress-event cursor. **Phase 1 writes the authorization
  record even though enforcement is still coarse**, so Phase 2 revocation wires straight in.

### 2.5 Phasing (implement Phase 1 only; rest is roadmap)

| Phase | Scope | Delivers |
|-------|-------|----------|
| **1 (this plan)** | `broad-audited` mode + deny-list overlay + explicit opt-in + `perm show` warning + `GovernanceState` authorization record + telemetry-on-enable | Research-agent unblock; least-privilege skeleton; audit hook present |
| 2 | Audit plane: structured per-egress event log tied to `GovernanceState`; Commander `revoke-egress` directive | Auditability + instant revocation (enterprise entry line) |
| 3 | Managed egress proxy (DLP, rate limit, per-request allow/deny); sbpl pins egress to proxy | Data-exfiltration control (true enterprise grade) |
| 4 | Per-tool egress scoping enforcement | Defense in depth |

### 2.6 Phase 1 acceptance

- `mur agent perm set-mode <a> network.outbound broad-audited` succeeds, requires the
  existing consent path, and records authorizer + timestamp in `GovernanceState`.
- `mur agent perm show <a>` prints a prominent `BROAD EGRESS ON` warning for that mode.
- A `broad-audited` agent can reach arbitrary hosts EXCEPT those on `deny_hosts`; a
  denied host is blocked (fail-closed).
- Enabling emits one telemetry/GovernanceState event.
- `unrestricted` and `restricted` behavior is unchanged (no regression).

## 3. Part B — CLI hardening (mechanical fixes)

Independent of Part A; clear correct-behavior fixes. One PR-sized batch.

| # | Fix | Correct behavior |
|---|-----|------------------|
| 1 | **`mur agent create --model <alias>` drops `model_ref`** (`cmd/agent/lifecycle.rs`, `resolve_model_ref_for_create`). It only reverse-maps `--provider X --model <realname>`; a bare alias falls to the `ollama` default with no `model_ref`. | If `--model <value>` matches a `~/.mur/models.yaml` alias, set `model_ref = <value>` and derive provider/name from the registry. |
| 2 | **`mur agent mcp add --arg --engine` clap-fails** on values starting with `--`. | Set `allow_hyphen_values` on the `--arg` option so `--arg --engine` parses natively; keep `--arg=<value>` working too. Add an example to the help.
| 3 | **`mur fleet add a,b` stores one bogus member.** | Split on commas as well as spaces; validate each name resolves to an existing agent; reject unknown names loudly. |
| 4 | **`mur skill new` scaffolds into CWD**, polluting the repo. | Default output to `~/.mur/skills/` (with `--dir` to override), matching where `skill list`/`scope` read from. |
| 5 | **No `mur agent import --as <name>`** — can't clone/rename. | Add `--as <name>` to `mur agent import` (or a `mur agent clone <src> <dst>`), regenerating identity, never copying the private key. |
| 6 | **No per-agent `mur agent doctor <name>`.** | Accept an optional agent arg that runs per-agent checks (model_ref resolves, MCP commands resolve on PATH, entitlements sane); keep the no-arg export-prereq behavior. |
| 7 | **`mur agent restart` non-variadic** (docs implied multi). NOTE: verify against current `main` — a `mur agent start` subcommand landed in #657; reconcile. | Accept multiple names (variadic), consistent with `fleet add`. |
| 8 | **`mur skill scope --fleet` errors unhelpfully** when no fleet name is given. | Improve the error to state `--fleet <NAME>` is required and the fleet need not exist yet. |
| G2 | **`install-service` plist has no `EnvironmentVariables.PATH`** (`agent_admin/lifecycle.rs`), so the launchd runtime can't find PATH-installed MCP binaries. | Write `EnvironmentVariables.PATH` into the plist, derived from the user's login-shell PATH (e.g. `npm config get prefix`/bin + common dirs), so bare-command MCP servers resolve. |

## 4. Component boundaries

| Unit | Responsibility |
|------|----------------|
| `NetworkOutboundMode` + `outbound` schema (`mur-common/agent.rs`) | Policy data model (Part A) |
| `GovernanceState.egress` (`mur-common`) | Authorization record + audit hook |
| sbpl policy builder (`mur-agent-runtime/sandbox`) | Enforce `broad-audited` = allow-all-minus-deny_hosts |
| `mur agent perm set-mode` / `show` (`mur-core/cmd/agent/perm`) | Enable + surface the mode |
| `cmd/agent/lifecycle.rs`, `cmd/agent/mcp.rs`, `cmd/fleet`, `cmd/skill_cmd.rs`, `agent_admin/lifecycle.rs` | Part B fixes |

## 5. Testing

- Part A: unit-test the sbpl policy builder for `broad-audited` (allows arbitrary host,
  blocks a `deny_hosts` entry); test `set-mode` writes the `GovernanceState` record;
  snapshot-test `perm show` output shows the warning.
- Part B: one focused test per fix — e.g. `create --model <alias>` yields `model_ref` set;
  `fleet add a,b` yields two validated members; `install-service` plist contains a PATH.

## 6. Out of scope

- Egress proxy / DLP / rate limiting (Phase 3).
- Per-request egress logging and Commander `revoke-egress` (Phase 2).
- Per-tool scoping enforcement (Phase 4) — schema carries the binding, enforcement later.
- Any change to `restricted` / `unrestricted` semantics.
