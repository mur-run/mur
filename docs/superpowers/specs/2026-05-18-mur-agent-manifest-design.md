# Agent Manifest — Design

**Status**: proposed
**Author**: david + Claude Opus 4.7
**Date**: 2026-05-18
**Related**: `plans/2026-05-18-continual-learning-versioned-evolution.md` (v2 spec §8.3, D6 / D7 / E6), `2026-04-29-model-registry-and-secret-refs-design.md`, `2026-04-22-murmur-p0-agent-runtime-design.md`

## Problem

Building a mur agent today is **imperative and scattered**. Recreating "the same agent" on another machine — or letting commander provision it remotely — requires running N CLI commands in a specific order:

```bash
mur agent create code-reviewer --type=chat --model-ref=anthropic/sonnet-4-6
mur agent perm allow-host code-reviewer github.com
mur agent perm allow-host code-reviewer sentry.io
mur agent perm allow-read code-reviewer /Users/david/Projects
mur agent skill add code-reviewer skills/review-rust.md
mur agent skill add code-reviewer skills/review-typescript.md
mur agent mcp add code-reviewer github --command npx ...
mur agent secret set code-reviewer GITHUB_TOKEN keychain:mur-github/work
mur agent prompt set code-reviewer < sys_prompt.md
mur agent companion add code-reviewer slack-bridge ...
```

This blocks five concrete workflows:

1. **Cross-machine deployment** — commander wants to push the same agent to 5 remote hosts. No single artifact to ship.
2. **GitOps for agents** — user wants `infra/agents/*.yaml` in a project repo with PR review on changes. No declarative artifact to put under review.
3. **Drift detection** — after weeks of ad-hoc CLI edits, no way to ask "is this agent still in the shape I intended?".
4. **E6 federation config** — the new pattern snapshot filter (`applies_in`, `tier`, `maturity`, `max_count`) needs a home. `profile.yaml` is the wrong layer (it's about runtime identity, not knowledge filtering).
5. **Cost & resource limits** — no per-agent token budget, no per-agent concurrent-session cap. Bill shock waiting to happen once a user runs 20 agents.

Existing `profile.yaml` is **runtime identity** (model, transport, entitlements, capabilities) — not the right shape to absorb skills, MCP servers, secrets bindings, federation policy, and resource limits. Expanding it would conflate concerns and break P0a's hot-path assumptions.

## Research consensus

Three reference points converge on the same shape:

- **Kubernetes manifests** — `apiVersion / kind / metadata / spec`, declarative, server reconciles state to match. Battle-tested across millions of clusters. Drift detection via `kubectl diff`.
- **GitHub Actions workflows** — single YAML per workflow, lives in git, applied by a controller, drives both local (`act`) and remote (Actions runners) execution from one source. Proves "same spec, multiple targets" works without coordination protocol.
- **Letta `AgentFile`** (2025 Q4 OSS) — declarative agent definition with memory blocks + tools + persona inline; portable across Letta server instances. Closest direct analogue, but ours must accommodate mur's existing `profile.yaml` + git layering.

The conclusion: **AgentManifest is additive over `profile.yaml`, not a replacement**. Existing agents work unchanged. Manifest is opt-in for users who want declarative ops. Future-proofs commander remote deployment (D6 in continual-learning v2 spec).

## Decisions

1. **`AgentManifest` is the declarative wrapper, `profile.yaml` remains the runtime artifact.** Apply flow: manifest → controller reconciles → writes/updates `profile.yaml`, `sys_prompt.md`, `skills/`, perms, MCP configs. Runtime never reads manifest directly; it reads the reconciled artifacts.

2. **`apiVersion: mur.run/v1`, `kind: AgentManifest`.** Future kinds (`WorkflowManifest`, `TeamManifest`, `PolicyManifest`) follow the same shape; share metadata structure via `mur-common::manifest::Metadata`.

3. **Manifest can mix inline + ref.** Small agents inline everything (one file, easy to read). Large agents reference external files (`skills_refs: [skills/x.md, skills/y.md]`). Reconciler resolves refs relative to manifest file's directory.

4. **Apply is idempotent + diffable.** `mur agent apply -f m.yaml` always produces the same state given the same input. `mur agent diff -f m.yaml` shows would-be changes without applying. `mur agent diff <name>` shows manifest vs current actual state (drift report).

5. **No templating / no admission controllers in v1.** Users who need templating use external tools (`envsubst`, helm-style). v1 is a flat schema. Future: jsonnet/CUE adapter as separate tool.

6. **Each apply produces an E1 commit.** Reconciler commits to `~/.mur/agents/.git` with reason `agent(<name>): apply manifest <kind> <delta-summary>`. Rollback = re-apply previous manifest revision (E1 history is the audit trail).

7. **`AgentManifest` is the source of truth for E6 federation config.** `spec.patterns.filter` and `spec.federation.*` live here, not in `profile.yaml`. profile.yaml stays small and runtime-focused.

8. **Commander uses the same schema.** `mur commander apply -f m.yaml --target=host:agent-name` ships the manifest to remote host and invokes `mur agent apply` there over A2A tunnel. Schema parity is enforced by both binaries depending on the same `mur-common::manifest` crate module.

9. **Secrets follow existing `SecretRef`** (2026-04-29 design). Manifest is commit-safe — never inline secret values.

10. **`mur agent describe <name> > m.yaml` exports current state.** Migration path for existing imperatively-created agents. Output is the manifest that, when applied, reproduces the current state exactly.

## §1 Architecture

```
~/.mur/
├── agents/
│   ├── .git/                          (E1 execution-layer repo)
│   └── <name>/
│       ├── manifest.yaml              ← NEW: source of truth (optional)
│       ├── profile.yaml               ← reconciled artifact (existing)
│       ├── sys_prompt.md              ← reconciled artifact
│       ├── skills/                    ← reconciled artifacts
│       ├── mcp/                       ← reconciled artifacts
│       └── identity.pub/.prev         ← runtime, not managed by manifest
```

```
mur-common::manifest                   (new module)
  ├── Metadata { name, workspace, annotations }
  ├── AgentManifest { api_version, kind, metadata, spec }
  ├── AgentSpec { profile, sys_prompt, skills, mcp_servers,
  │              patterns, resources, entitlements, federation,
  │              companion, secrets }
  └── validation / diff / merge helpers

mur-core::manifest                     (new module — reconciler)
  ├── reconcile(manifest) → ChangeSet
  ├── apply(change_set) → Result<CommitId>
  ├── diff(manifest, actual) → Diff
  └── describe(agent_name) → AgentManifest

mur-core::cmd::agent::apply.rs         ← `mur agent apply -f m.yaml`
mur-core::cmd::agent::diff.rs          ← `mur agent diff [-f m.yaml] <name>`
mur-core::cmd::agent::describe.rs      ← `mur agent describe <name>`
mur-core::cmd::agent::validate.rs      ← `mur agent validate -f m.yaml`
```

**Apply dataflow** (local):

```
1. Read manifest file, resolve refs relative to file dir
2. Validate schema (JSON Schema) + semantic (referenced files exist, model_ref resolves,
   entitlements parse, secret refs parse)
3. Load current agent state from ~/.mur/agents/<name>/ (or empty if new)
4. Compute ChangeSet: profile.yaml diff, sys_prompt.md diff, skills/ add/remove,
   mcp/ add/remove, perms diff, federation config diff
5. If --dry-run: print Diff and exit
6. Reconcile: write all artifacts atomically (tempdir + rename), run
   `mur agent stop` if model/transport/entitlements changed (require restart)
7. Commit to ~/.mur/agents/.git with reason `agent(<name>): apply <summary>`
8. If agent was running and not stopped above, send SIGHUP to reload (skills + prompt
   hot-reload supported; profile/entitlements changes need full restart)
```

**Apply dataflow** (commander remote):

```
1-2. Same (run on commander control plane)
3. Open A2A tunnel to target host's mur-daemon
4. POST /v1/agents/apply with manifest body + target name
5. Remote daemon runs steps 3-8 from local flow above
6. Stream reconciliation log back to commander; commander records to audit store
```

## §2 Schema

`~/.mur/agents/<name>/manifest.yaml` (canonical example, all fields):

```yaml
apiVersion: mur.run/v1
kind: AgentManifest

metadata:
  name: code-reviewer
  workspace: default                   # defaults to "default"
  annotations:
    owner: david@example.com
    purpose: "Review PRs in Mur project"
  labels:                              # for `mur agent list -l owner=david`
    owner: david
    env: dev

spec:
  # ─── Identity & runtime ──────────────────────────────────────
  display_name: "Code Reviewer"
  model_ref: anthropic/sonnet-4-6      # resolves via models.yaml
  transport: a2a                        # a2a | stdio | http
  capabilities:
    streaming: true
    tools: true

  # ─── Prompt & skills ─────────────────────────────────────────
  sys_prompt: |                         # inline form
    You are a senior code reviewer focused on Rust and TypeScript...
  # — or —
  # sys_prompt_ref: ./prompts/code-reviewer.md

  skills:                               # inline list of skill objects
    - name: review-rust
      content_ref: ./skills/review-rust.md
    - name: review-typescript
      content: |
        # Review TypeScript
        Focus on...

  # ─── MCP servers ─────────────────────────────────────────────
  mcp_servers:
    - name: github
      command: npx
      args: ["-y", "@modelcontextprotocol/server-github"]
      env:
        GITHUB_PERSONAL_ACCESS_TOKEN: ${secrets.GITHUB_TOKEN}

  # ─── Knowledge federation (E6) ───────────────────────────────
  patterns:
    filter:
      applies_in: [code, review]
      applies_to_projects: [mur, dashboard]
      tier: [project, core]
      maturity: [stable, canonical]
      importance_min: 0.5
      max_count: 200
    snapshot_policy: pull-on-start      # pull-on-start | pull-periodic | manual
    snapshot_interval_minutes: 60       # only if policy=pull-periodic

  # ─── Resource governance ─────────────────────────────────────
  resources:
    token_budget_per_day_usd: 5.00      # hard cap; over → agent refuses new turns
    max_concurrent_sessions: 3
    max_context_tokens: 200000

  # ─── Entitlements (P0a sandbox) ──────────────────────────────
  entitlements:
    network:
      hosts: [api.github.com, sentry.io, api.anthropic.com]
    filesystem:
      read: [/Users/david/Projects/mur]
      write: []
    spawn:
      allowed: [git, cargo, rustfmt]
    limits:
      cpu_percent: 50
      memory_mb: 2048

  # ─── Federation (E6) ─────────────────────────────────────────
  federation:
    evidence_outbox_max_age_minutes: 15
    snapshot_diff_threshold_pct: 20     # > N% pattern change → require human ack

  # ─── Companion (Slack/TG/Discord bridges, C7) ────────────────
  companion:
    enabled: true
    channels:
      - kind: slack
        team_ref: keychain:mur-slack/team1
      - kind: telegram
        bot_ref: keychain:mur-telegram/bot

  # ─── Secrets binding ─────────────────────────────────────────
  secrets:
    GITHUB_TOKEN: keychain:mur-github/work
    OPENAI_API_KEY: env:OPENAI_API_KEY
    # See 2026-04-29 design for full SecretRef codec

  # ─── Lifecycle hooks ─────────────────────────────────────────
  hooks:
    on_apply:                            # runs after reconcile, before restart
      - run: ./scripts/post-apply.sh
    on_first_start:
      - mur: "agent send code-reviewer 'Initialized.'"
```

### Schema versioning

- `apiVersion: mur.run/v1` is frozen once shipped. Field deprecations: keep + warn for 2 minor versions, then `mur.run/v2`.
- Reconciler accepts `mur.run/v1` indefinitely; unknown future fields under v1 are **rejected** (strict parse) to avoid silent drift.
- `mur agent validate` reports unknown fields with helpful suggestions ("did you mean `entitlements.network.hosts`?").

### Minimal manifest

For a simple agent:

```yaml
apiVersion: mur.run/v1
kind: AgentManifest
metadata:
  name: scratch
spec:
  model_ref: anthropic/haiku-4-5
  sys_prompt: "You are a quick helper."
```

All other fields default. `transport: stdio`, no skills, no MCP, no federation, no companion, no entitlements override (uses workspace default policy).

## §3 CLI

```
mur agent apply -f <manifest.yaml> [--dry-run] [--no-restart] [--force]
mur agent apply -f <dir>/                     # recursive apply on a directory
mur agent diff [-f <manifest.yaml>] <name>    # vs manifest, or manifest vs current
mur agent describe <name> [-o yaml|json]      # export current state → manifest
mur agent validate -f <manifest.yaml>         # schema + semantic check, no side effect
mur agent delete -f <manifest.yaml>           # remove agent declared in manifest
                                              # (--cascade also removes ~/.mur/agents/<name>/)
mur agent list -l owner=david                  # label selector (k8s style)

mur commander apply -f <manifest.yaml> --target=<host[:agent-name]>
mur commander diff <host>:<name>
mur commander list-deployments
```

**Important defaults:**

- `mur agent apply` without `--no-restart` will stop & restart agent if profile/entitlements/transport changed. With `--no-restart`, changes are written to disk but daemon defers restart until next manual stop/start.
- `--dry-run` prints unified diff of all reconciled artifacts; exits 0 if no changes, 2 if changes pending.
- `--force` bypasses the "manifest_revision in metadata differs from last-applied" optimistic-concurrency check (default behavior prevents stomping on parallel apply).

## §4 Reconciler details

### §4.1 ChangeSet computation

```rust
pub struct ChangeSet {
    pub agent_name: String,
    pub profile_changes: ProfileDiff,            // model_ref, transport, capabilities
    pub sys_prompt_change: Option<TextDiff>,
    pub skill_changes: SkillSetDiff,             // added, removed, modified
    pub mcp_changes: McpSetDiff,
    pub perm_changes: PermDiff,
    pub federation_changes: FederationDiff,
    pub companion_changes: CompanionDiff,
    pub secret_changes: SecretBindingDiff,
    pub requires_restart: bool,                  // profile/entitlements/transport touched
    pub requires_snapshot_refresh: bool,         // E6: patterns.filter touched
}
```

### §4.2 Apply order (deterministic, fail-safe)

1. **Validate** entire manifest. Bail on any error before touching disk.
2. **Stop** agent if `requires_restart` and agent is running. Persist intent (`~/.mur/agents/<name>/.apply-in-progress`) so an interrupted apply can resume.
3. **Write artifacts** to tempdir (`~/.mur/agents/<name>/.apply-staging/`). Atomic rename per file.
4. **Update perms / entitlements** in profile.yaml.
5. **Reconcile companion** (notify companion controller of channel changes).
6. **Refresh snapshot** if `requires_snapshot_refresh`: invoke E6 `federation::pull_snapshot`. On failure, keep old snapshot, log warning, continue.
7. **Commit** to `~/.mur/agents/.git`.
8. **Restart** agent if needed (read fresh profile).
9. **Run hooks** (`on_apply` then, if first start, `on_first_start`).
10. Remove `.apply-in-progress` marker.

If step 2-9 fails after step 7 (commit): the commit stays (auditable record of intent), but a follow-up `agent(<name>): apply failed at <stage>` commit is written with the partial state. Operator sees both in `mur agent history`.

### §4.3 Drift detection (`mur agent diff <name>`)

1. `describe(<name>)` → current state as `AgentManifest`
2. Read declared manifest from `~/.mur/agents/<name>/manifest.yaml`
3. Compute `Diff` (same engine as apply)
4. Report. Exit 0 if no drift, 3 if drift detected (CI-friendly).

If `manifest.yaml` is absent (imperatively-created agent), report "no manifest declared — run `mur agent describe <name> > manifest.yaml` to adopt declarative mode".

## §5 Commander integration

### §5.1 Wire protocol

Commander → mur daemon over A2A tunnel (existing transport). New endpoint:

```
POST /v1/manifest/apply
  Headers:
    Authorization: Bearer <commander-token>     (D4 from continual-learning spec)
    Content-Type: application/yaml
  Body: <AgentManifest YAML>
  Response (streaming):
    event: validate    data: { ok: true }
    event: changeset   data: { ... }
    event: stage       data: { name: "stop", status: "ok" }
    event: stage       data: { name: "write", status: "ok" }
    ...
    event: commit      data: { sha: "abc123", reason: "..." }
    event: done        data: { agent: "code-reviewer", restarted: true }
```

Commander records every event to its own audit store (per-host, per-deployment).

### §5.2 Multi-target deployment

`mur commander apply -f m.yaml --target=host1,host2,host3`:

- Iterates targets serially by default (one host fails → halt, don't cascade).
- `--parallel=N` for fan-out (use with care).
- `--rollout=canary --canary-pct=10` for staged rollout (future, not v1).

### §5.3 Commander-side state

Commander stores per-host **applied manifest digest** (not the manifest itself — that lives on the target). On next apply, commander computes manifest digest, compares, skips if unchanged (`--force` to bypass). This avoids re-applying identical manifests to hundreds of hosts.

## §6 Validation rules

Semantic checks beyond schema:

| Rule | Failure |
|---|---|
| `metadata.name` matches `^[a-z][a-z0-9-]{1,62}$` | reject |
| `spec.model_ref` resolves in `~/.mur/models.yaml` | reject (or warn if `--allow-unresolved`) |
| `spec.sys_prompt_ref` file exists, readable | reject |
| Each `spec.skills[].content_ref` file exists | reject |
| `spec.secrets.*` parses as valid `SecretRef` | reject |
| `spec.entitlements.network.hosts` are valid hostnames or IPs | reject |
| `spec.entitlements.spawn.allowed` are non-empty, single-word executable names | reject |
| `spec.patterns.filter.tier` ⊆ `[session, project, core]` | reject |
| `spec.patterns.filter.maturity` ⊆ `[draft, emerging, stable, canonical]` | reject |
| `spec.resources.token_budget_per_day_usd` >= 0 | reject |
| `spec.companion.channels[].kind` ⊆ supported kinds | reject |
| Unknown top-level field under `spec.` | reject (strict) |

## §7 Migration

### §7.1 Adopting manifests for existing agents

```bash
mur agent describe my-existing-agent > ~/.mur/agents/my-existing-agent/manifest.yaml
mur agent diff my-existing-agent                # should be empty
git -C ~/.mur/agents add my-existing-agent/manifest.yaml
git -C ~/.mur/agents commit -m "agent(my-existing-agent): adopt manifest"
```

After this, edits go through `mur agent apply` (or direct edit + apply). CLI commands like `mur agent perm allow-host` still work — they re-render the manifest and re-apply, so the manifest stays in sync.

### §7.2 Detecting "imperative drift"

After adopting a manifest, if user runs `mur agent perm allow-host my-agent example.com` (imperative), the CLI:

1. Loads current manifest
2. Adds entitlement to in-memory copy
3. Writes back manifest.yaml
4. Calls `mur agent apply -f manifest.yaml`

→ No drift. Manifest stays canonical.

If user **directly edits** `profile.yaml`:

- Next `mur agent diff` detects drift, suggests `mur agent describe > manifest.yaml` to re-adopt or `mur agent apply -f manifest.yaml` to revert.

### §7.3 Removing manifest mode

```bash
rm ~/.mur/agents/<name>/manifest.yaml
```

Agent reverts to imperative mode. No state lost (artifacts still on disk).

## §8 Open questions

1. **Apply ordering across multiple manifests** — when applying a directory, should agents that reference each other (e.g., `companion.peer_agent_ref`) be ordered? **v1 answer**: no inter-manifest refs in v1; defer to v2.
2. **Workspace concept** — `metadata.workspace` is declared but currently `~/.mur/agents/` is flat. Should we move to `~/.mur/workspaces/<ws>/agents/<name>/`? **v1 answer**: keep flat; workspace is a label/filter only. Migrate to dir structure in v2 if multi-workspace becomes load-bearing.
3. **Secret resolution in manifest at apply time** — should we resolve secrets at apply time to verify they exist? **v1 answer**: yes, with `--skip-secret-check` opt-out (useful when applying on a control plane without the runtime secrets). Resolution result is **not stored**; only "does it resolve" boolean.
4. **Manifest in agent's own git** — when commander applies remotely, the target host's `~/.mur/agents/.git` gets the commit. Should commander also keep a copy in its own audit store? **v1 answer**: yes, commander stores the manifest body in audit store keyed by `(host, agent, manifest_digest, applied_at)`. Allows commander-side rollback even if target host is gone.

## §9 Non-goals (explicit)

- **Templating** — no `{{ .Values.x }}` in v1. Use `envsubst` externally.
- **Inheritance / overlays** — no `base: parent-manifest.yaml`. v1 is flat.
- **Schema migrations between v1 and future v2** — out of scope (handled by `mur agent migrate-manifest` when v2 ships).
- **Cron / scheduled apply** — out of scope (use system cron + `mur agent apply`).
- **Manifest as runtime config** — runtime always reads reconciled artifacts. Performance reason: no YAML parse + ref resolution + validation in hot path.

## §10 References

- Continual-learning v2 spec — `plans/2026-05-18-continual-learning-versioned-evolution.md` §8.3 (D6), §9 (E6)
- P0a agent runtime — `docs/superpowers/specs/2026-04-22-murmur-p0-agent-runtime-design.md`
- Model registry & SecretRef — `docs/superpowers/specs/2026-04-29-model-registry-and-secret-refs-design.md`
- Commander memory sync — `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md`
- Kubernetes — [API conventions](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md), [kubectl apply semantics](https://kubernetes.io/docs/reference/using-api/server-side-apply/)
- Letta AgentFile — [letta-ai/letta repo](https://github.com/letta-ai/letta)
