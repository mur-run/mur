# M7c — Automatic Propagation + Credit (Design)

**Status:** Design draft. Authored 2026-05-26 after M7a shipped (PR #284, merged 2026-05-26) and M7b designed (`docs/superpowers/specs/2026-05-26-mur-skill-ecosystem-m7b-design.md`, plan `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7b.md`). M7c is the third and final M7 sub-milestone — it closes the cross-agent evolution loop by automating skill discovery, attaching credit to each contribution, and harmonising intent vocabulary across the host.

**Spec mapping:** §M7 cross-agent evolution (credit / reputation + automatic propagation halves), §10.1 evolution tracking (extends with lineage ledger), §M6b intent vocabulary (cross-agent canonical mapping).

**Scoping doc:** `docs/superpowers/plans/2026-05-26-mur-skill-ecosystem-m7-scoping.md` §3 M7c. Resolves scoping Q5 (credit), Q6 (trust inheritance), Q7 (intent canonicaliser ownership).

**Hard dependencies:**
- M4b — `agent://` pull install path (`cmd::skill_install::cmd_install`) and `transfer_chain` mutation on entry.
- M5a — `SkillStats` per-agent path (`SkillStats::path_agent`) for fitness inputs.
- M5b — idle-hook plumbing (`skill-sweep` is the existing precedent for an agent-owned background sweep task; `skill-propagate` follows the same pattern).
- M6a — schema validator (re-used when inheriting a skill from a peer).
- M6b — `ProcedureStep::intent` field (canonicaliser writes a lookup the inject path consults).
- M6c — idle hook wiring via C6 (`mur agent schedule idle-add`).
- M7a — `list_peer_agents`, `AgentFitness`, `list_installed_agent`, cross-agent stats aggregation.
- M7b — `EvolutionEvent::Recombined` (recombination contributions feed the credit ledger).

**Soft dependency:** None. Semantic clustering for the intent canonicaliser is explicitly deferred (frequency-only in v1).

---

## 1. Goal

M7a let an agent **see** peers' skills and their performance. M7b let an agent **breed** new skills from cross-agent parents. M7c lets an agent **inherit** high-fitness skills from its peers without a human in the loop, while recording who contributed what.

Three user-visible behaviours ship together because they share data:

1. **Propagation** — an agent periodically scans peers, finds skills with high population fitness that it lacks, and installs them locally as `Sandboxed Draft`. Pull-side only; never writes to peer state (preserves the M7a invariant).
2. **Credit lineage** — every install, evolve, recombine, and propagate event appends to a per-agent append-only ledger. `mur skill credit <name>` shows who originally authored a skill, who has mutated it, and which agents propagated it. No token. Fitness weight is the only incentive.
3. **Intent canonicaliser** — a host-level mapping that resolves divergent intent strings ("search the web", "web_search", "lookup_web") to a single canonical form so cross-agent transfer and consolidation stay coherent.

Everything M7c writes is reversible: stop the idle hook, delete the local skill, delete the ledger file — peer state is untouched.

## 2. Non-goals

- Pushing skills to peers (no A2A `skill.invite` method; propagation is strictly pull-side).
- Remote-host peer discovery — `peers.yaml`, hostname-based dial, configured remotes (carried out of scope per scoping §7).
- Trust-level changes on inheritance — the skill enters at `Sandboxed Draft` even when both agents are mutually `Trusted` (re-evaluated in a future milestone, not this one).
- Token / currency for credit — confirmed as scoping Q5; leaderboard via fitness weight is sufficient for v1.
- Semantic / embedding-based intent clustering — frequency only in v1; embedding clustering is M7+.
- Cross-agent contradiction detection — deferred per M7b §12 (legitimate divergence is a feature, not a bug).
- N-way credit attribution beyond `(author, mutator, recombiner, propagator)` — single-axis labels, no weights, no fractional credit.
- Automatic promotion of inherited skills past `Draft` — the existing M3c evolve / M5b consolidate / M5a stats pipeline handles maturity, unchanged.
- Modifying `SkillManifest` schema — lineage and credit live in sidecar files so the signature-scoped manifest stays stable.

## 3. Design decisions

These are decisions made during the design pass; each is load-bearing.

### 3.1 Propagation is pull-side only

The scoping doc said "offer to push via `agent://`." After modelling it against M7a's invariants, push is the wrong shape:

- M4b's wire is a pull (`install agent://<peer>/<skill>`). There is no A2A method to *write* a skill into a peer.
- M7a's safety invariant is "never modify peer state." A push would break it.
- A pull-side discovery scan run by each agent on its own schedule is symmetric, requires no new wire methods, and stays inside the invariant.

So `mur agent propagate` runs **from the invoking agent's perspective**: scans peers, picks skills the invoker lacks (or has a lower-fitness version of), installs them locally via the existing M4b `install_from_agent` path. The word "propagate" describes the population-level outcome over time — agents independently pulling high-fitness skills from each other — not a single push action.

Concretely, one sweep does:

```
for each peer p in list_peer_agents(home) where p != self:
  for each skill s in list_installed_agent(home, p):
    if local already has s with version >= peer's version: skip
    aggregate population_fitness(s) across all peers (M7a stats_agg)
    if population_fitness(s) >= threshold AND
       peer p has the version with highest per-agent fitness for s:
      pull via cmd_install(home, registry_url, "agent://p/s")
```

The "highest per-agent fitness" check resolves multi-version: if alice has `research-prices v1.2` at 0.8 fitness and bob has `research-prices v1.0` at 0.4 fitness, the propagation source is alice. Deterministic: tiebreak by `AgentFitness.weight`, then alphabetical agent name (same hierarchy M7b §3.5 uses).

### 3.2 Propagation threshold + safety gates

Three gates in order, all configurable:

| Gate | Default | Config key |
|---|---|---|
| Skill must have ≥ `min_samples` total usage across all peers | 5 | `propagate.min_samples` |
| `population_fitness(skill)` ≥ `min_fitness` | 0.7 | `propagate.min_fitness` |
| Source agent's `AgentFitness.weight` ≥ `min_source_weight` | 0.3 | `propagate.min_source_weight` |

A single peer with one lucky success does not propagate. Three peers each with 5 successes and 1 failure can. Tunable via `~/.mur/config.yaml`:

```yaml
propagate:
  enabled: true
  min_samples: 5
  min_fitness: 0.7
  min_source_weight: 0.3
  max_per_sweep: 3       # cap installs per sweep to avoid storms
  exclude_patterns: []   # name glob patterns to never inherit
```

`max_per_sweep` caps a single run; the idle hook fires periodically so a backlog drains over multiple ticks.

`exclude_patterns` is the user's veto. If `secrets-*` is excluded, no peer skill matching that glob is ever auto-installed. Defaults to empty.

### 3.3 Inheritance trust + lifecycle

Per scoping Q6, confirmed: inherited skills always enter as:

- **Lifecycle:** `Draft` (same as a fresh `mur skill install agent://...` today — M4b sets nothing special, but the install path goes through `scan_skill` which can downgrade trust on findings)
- **Trust:** `Sandboxed`, regardless of source agent's trust level
- **`transfer_chain` mutation:** existing M4b code already appends `agent://<source>`; M7c does not change this

The existing M3c evolve / M5b consolidate / M5a stats pipeline observes usage on the inheriting agent and promotes via the normal Draft → Emerging → Stable → Canonical path. Propagation is just bulk install + idle automation; the install itself is unchanged from M4b.

### 3.4 Credit ledger: per-agent JSONL, append-only

Layout:

```
~/.mur/agents/<agent>/credit/ledger.jsonl
```

One JSON object per line. Append-only — never rewritten in place. Schema:

```jsonc
// 2026-05-27T10:21:33Z author of a brand-new skill
{
  "ts": "2026-05-27T10:21:33Z",
  "skill": "research-prices",
  "skill_version": "1.0.0",
  "kind": "author",
  "agent": "alice",            // crediting subject (the contributor)
  "evidence": null,
  "source": "human:alice"      // mirrors EvolutionEvent.source
}
// 2026-05-27T11:02:15Z bob inherited via propagate
{
  "ts": "2026-05-27T11:02:15Z",
  "skill": "research-prices",
  "skill_version": "1.0.0",
  "kind": "propagator",
  "agent": "bob",
  "evidence": {
    "from_agent": "alice",
    "fitness_at_install": 0.78,
    "samples_at_install": 7
  },
  "source": "agent://alice"
}
// 2026-05-27T13:44:02Z bob ran skill recombine; alice contributed parent A
{
  "ts": "2026-05-27T13:44:02Z",
  "skill": "research-x-lookup",
  "skill_version": "1.0.0",
  "kind": "recombiner",
  "agent": "alice",
  "evidence": { "role": "parent_a", "child": "research-x-lookup" },
  "source": "agent://alice"
}
// 2026-05-28T09:00:00Z alice ran skill evolve on research-prices
{
  "ts": "2026-05-28T09:00:00Z",
  "skill": "research-prices",
  "skill_version": "1.1.0",
  "kind": "mutator",
  "agent": "alice",
  "evidence": { "from_version": "1.0.0", "diff_summary": "added retry step" },
  "source": "human:alice"
}
```

Four `kind` values: `author`, `mutator`, `recombiner`, `propagator`. Closed enum. New kinds are additive (read-side must tolerate unknown kinds — log + skip).

Per-agent files mean: each agent only writes to its own ledger. `mur skill credit <name>` aggregates by reading peer ledgers (read-only — same pattern M7a uses for cross-agent stats).

**Why per-agent and not host-level**: the `~/.mur/agents/<self>/` directory is the established write boundary. A host-level `~/.mur/credit/` would create a multi-writer file (concurrency, ownership, deletion semantics all murky). Per-agent files preserve the same boundary as `evolution.log`.

**Wiring points** — append to the local ledger from these existing code paths:

| Where | Kind | Trigger |
|---|---|---|
| `cmd::skill_install::install_from_agent` (M4b) | `propagator` (if invoked via idle hook) / `author` for non-`agent://` registry/local installs treated as `author` on the installing agent | install completes |
| `cmd::skill_install::cmd_install` non-agent path | `author` | install completes |
| `evolve::skill_evolve` (M3c) | `mutator` | evolve completes successfully |
| `cmd::skill_recombine` (M7b) | `recombiner` (×2 — one per parent) + `author` for the invoking agent (the offspring is theirs to own) | recombine completes |
| `cmd::skill_from_pattern` (M5a) | `author` | local-from-pattern skill created |
| `cmd::skill_generate` (M3c) | `author` | generated-from-session skill created |

All ledger writes go through a single helper (`credit::ledger::append`) that does atomic append (`open(O_APPEND)`, single `write_all`, fsync optional via config). On write failure the operation does **not** roll back the underlying install/evolve — losing a ledger line is preferable to losing the skill.

### 3.5 Distinguishing `propagator` from `author` on install

Today's `cmd_install` path does not know whether it was invoked manually or by the idle hook. M7c adds an `InstallContext` enum threaded through:

```rust
pub enum InstallContext {
    /// User typed `mur skill install ...`
    Manual,
    /// Triggered by `skill-propagate` idle hook
    AutoPropagate { source_fitness: f64, source_samples: u64 },
}
```

`InstallContext::Manual` from CLI → `kind: "author"` for non-`agent://` installs, `kind: "propagator"` with empty fitness evidence for `agent://` installs.

`InstallContext::AutoPropagate { .. }` always → `kind: "propagator"` with fitness evidence populated.

This is a tiny signature change on `cmd_install` (one extra parameter, defaulted at call sites). Not a breaking change to the manifest.

### 3.6 Intent canonicaliser: host-level, frequency-based

File: `~/.mur/intent_canonical.yaml`.

Shape:

```yaml
version: 1
generated_at: 2026-05-27T12:00:00Z
generated_by: alice          # last agent that ran the canonicaliser
canonical:
  - canonical: web_search
    aliases: [search_web, web search, search the web, web-search]
    count: 14                # total occurrences across all peers
  - canonical: open_url
    aliases: [navigate_to, browser.navigate]
    count: 8
```

**Clustering** (v1):

1. Normalize each intent string: lowercase, replace `[\s\-]+` with `_`, strip leading/trailing `_`.
2. Group by normalized form.
3. Within a group, the canonical is the **most frequent original spelling**, tiebreak alphabetical.
4. Aliases are all distinct original spellings in the group (including the canonical).

Embedding-based clustering (catches paraphrasing without identical normalised form) is M7+ — same rule M7b applied to gene diff.

**Ownership** (scoping Q7): any agent can rebuild the file. Last-writer-wins via atomic temp+rename. The `generated_by` field is informational.

**Read-side integration**: M6b's `injector` (when consulted at inject time) checks the canonical mapping before emitting an intent string. If a step's intent matches an `alias`, it's rendered as the `canonical`. The original string in the manifest is untouched — only the inject-time projection changes. This means the canonicaliser is non-destructive: rebuild it differently and inject output shifts; the manifest never does.

**CLI** for the canonicaliser:

```
mur skill intent canonicalise [--dry-run] [--json]
mur skill intent show               # print the current mapping
```

No `--apply` flag: `canonicalise` writes by default, `--dry-run` is the opt-out.

### 3.7 Idle hook: `skill-propagate`

The existing C6 idle scheduler runs registered triggers on a tick. M5b uses it for `skill-sweep` (lifecycle maturity sweep). M7c adds `skill-propagate` the same way:

- Trigger name: `skill-propagate`
- Default `after_secs`: 1800 (30 min idle)
- Default `cooldown_secs`: 7200 (2 hr)
- Default `respect_quiet_hours`: true
- Message dispatched to `TaskRunner`: a structured `propagate.run` task that calls `cross_agent::propagate::run_propagate(home, agent, opts)` directly (no LLM round-trip)

A bootstrap helper `mur agent schedule propagate-init <agent>` registers the trigger with sensible defaults — same UX as the M5b sweep initialiser. Manual override possible via the existing `idle-add` command.

The CLI form `mur agent propagate` runs the same `run_propagate` synchronously for ad-hoc use.

### 3.8 What lives in `SkillManifest` — nothing new

The scoping doc said "extend `transfer_chain` with `mutation_events: Vec<MutationEvent>`." After M7b chose to leave the manifest alone (manifest signature stability), M7c follows the same principle:

- **No new manifest fields.** Lineage data lives in `credit/ledger.jsonl` and `EvolutionEvent` (already in `evolution.log`).
- `mur skill credit` aggregates by reading both for the named skill across peers.
- This keeps the manifest signature stable across M7. Signing/publishing flow is unaffected.

### 3.9 Concurrency + atomicity

Two scenarios to handle:

- **Concurrent propagate sweeps on the same agent**: prevented by a `<home>/agents/<self>/credit/.propagate.lock` advisory lock (`fcntl(LOCK_EX | LOCK_NB)`). A second sweep returns immediately with exit code 7 ("propagate already running").
- **Concurrent canonicaliser writes from two agents on the same host**: tolerated by atomic temp+rename. Last writer wins; the loser's work was based on an older snapshot and the file is regenerated periodically anyway. No lock.

Ledger append is single-process per agent (the agent's runtime is the only writer to its own ledger). No lock needed; `O_APPEND` on POSIX is atomic for writes under PIPE_BUF.

---

## 4. Module structure

```
mur-common/src/skill/
  credit.rs                       # CreditKind, CreditEntry, CreditEvidence — pure data + serde

mur-core/src/cross_agent/
  propagate/
    mod.rs                        # pub fn run_propagate(home, agent, opts) -> PropagateReport
    candidates.rs                 # enumerate (peer, skill) pairs that pass fitness gates
    install_ctx.rs                # InstallContext enum + helpers
  credit/
    ledger.rs                     # append, read, scan-across-peers
    aggregate.rs                  # build a credit view for a single skill
  intent/
    canonical.rs                  # normalize, cluster, write yaml
    inject_lookup.rs              # read-side lookup used by M6b injector

mur-core/src/cmd/
  agent_propagate.rs              # `mur agent propagate` CLI
  skill_credit.rs                 # `mur skill credit <name>` CLI
  skill_intent.rs                 # `mur skill intent {canonicalise|show}` CLI

mur-core/src/cmd/skill_install.rs # +InstallContext arg, write to ledger on success
mur-core/src/evolve/skill_evolve.rs # +ledger append on successful evolve
mur-core/src/cmd/skill_recombine.rs # (M7b) +ledger appends on recombine (3 entries: author + 2 recombiners)
mur-core/src/cmd/skill_from_pattern.rs # +ledger append
mur-core/src/cmd/skill_generate.rs  # +ledger append

mur-core/src/cli/
  agent.rs                        # add Propagate subcommand variant
  skill.rs                        # add Credit, Intent subcommand variants

mur-agent-runtime/src/task_runner.rs # support `propagate.run` task kind invoked by idle hook
```

Projected line counts (all well under the 800-line ceiling):

| File | Lines | Notes |
|---|---|---|
| `credit.rs` (common) | 160 | Types + serde |
| `propagate/mod.rs` | 220 | Orchestration |
| `propagate/candidates.rs` | 200 | Gate enforcement + per-skill best-source selection |
| `propagate/install_ctx.rs` | 80 | Enum + helpers |
| `credit/ledger.rs` | 200 | Append + scan |
| `credit/aggregate.rs` | 180 | View builder |
| `intent/canonical.rs` | 240 | Normalize + cluster + write |
| `intent/inject_lookup.rs` | 120 | Read-side helper |
| `cmd/agent_propagate.rs` | 180 | CLI |
| `cmd/skill_credit.rs` | 200 | CLI + table renderer |
| `cmd/skill_intent.rs` | 160 | CLI |
| `cli/agent.rs` | +25 | Additive |
| `cli/skill.rs` | +35 | Additive |
| `cmd/skill_install.rs` | +40 | Signature change + ledger hook |
| `evolve/skill_evolve.rs` | +20 | Ledger hook |
| Other ledger-hook edits | +15 each | Tiny additive calls |
| `agent_runtime::task_runner` | +60 | New task kind |

---

## 5. CLI surface

### 5.1 `mur agent propagate`

```
mur agent propagate [options]

Options:
  --agent <name>                   invoking agent (defaults to current agent context;
                                   required outside an agent process)
  --dry-run                        scan and report; install nothing
  --max <n>                        override propagate.max_per_sweep for this run
  --min-fitness <f>                override propagate.min_fitness
  --min-samples <n>                override propagate.min_samples
  --json                           emit JSON outcome
```

Output (human):

```
Scanned 3 peers, found 7 candidate skills.
Gates: min_samples=5  min_fitness=0.7  min_source_weight=0.3  max_per_sweep=3

Installed (3):
  research-prices       v1.2.0  ← agent://alice  (fitness 0.78, n=7)
  parse-receipt         v0.4.1  ← agent://bob    (fitness 0.71, n=12)
  scrape-product-page   v1.0.0  ← agent://alice  (fitness 0.74, n=6)

Skipped (4):
  poll-imap             (fitness 0.55 < 0.70)
  redact-pii            (fitness 0.83 but only 3 samples < 5)
  archive-emails        (exists locally at same version)
  send-sms              (matches exclude_patterns: 'sms-*')
```

Exit codes:

| Code | Condition |
|---|---|
| 0 | Sweep completed (may have installed 0) |
| 4 | No peers found (informational; not always an error — single-agent host is valid) |
| 5 | Inheritance failed mid-sweep (one or more installs errored; report still emitted) |
| 7 | Propagate lock held — another sweep is in progress |

### 5.2 `mur skill credit <name>`

```
mur skill credit <name> [options]

Options:
  --agent <name>                   crediting view from this agent's perspective
                                   (defaults to current agent)
  --json                           emit JSON
  --since <duration>               only entries newer than this (e.g. "30d")
```

Output (human):

```
Skill: research-prices  (current version 1.2.0)

Author:
  alice    2026-05-20T10:21:33Z  source: human:alice

Mutators (3):
  alice    2026-05-22T09:10:01Z  v1.0.0 → v1.1.0  ("added retry step")
  alice    2026-05-23T14:08:47Z  v1.1.0 → v1.2.0  ("widened User-Agent header")
  bob      2026-05-25T11:30:12Z  v1.2.0 → v1.2.1  ("fixed JSON path")  [local-only]

Recombiners:  (none)

Propagators (4):
  bob      2026-05-21T11:02:15Z  v1.0.0  ← agent://alice  (fitness 0.78)
  carol    2026-05-22T13:48:00Z  v1.1.0  ← agent://alice  (fitness 0.81)
  bob      2026-05-23T16:11:55Z  v1.2.0  ← agent://alice  (fitness 0.84)
  carol    2026-05-23T17:00:12Z  v1.2.0  ← agent://alice  (fitness 0.84)

Lineage summary: 1 author, 3 mutators, 0 recombiners, 4 propagations across 2 peers.
```

### 5.3 `mur skill intent canonicalise` / `show`

```
mur skill intent canonicalise [--dry-run] [--json]
mur skill intent show
```

`canonicalise` rebuilds `~/.mur/intent_canonical.yaml` from a full host sweep. `--dry-run` prints the projected file to stdout without writing.

`show` prints the current file content (or a "no canonical mapping yet" message).

Exit codes follow the same pattern as M7b — 0 success, 2 missing inputs, 5 write failure.

---

## 6. Data flow

### 6.1 Propagation sweep

```
run_propagate(home, agent, opts):
  acquire <home>/agents/<agent>/credit/.propagate.lock  // exit 7 if held

  peers = list_peer_agents(home).filter(p != agent)
  if peers is empty: emit report and exit 4

  candidates = candidates::enumerate(home, agent, peers)
      // for each (peer, skill) where local lacks an equal-or-higher version:
      //   - load peer's SkillStats, sum into population_fitness via stats_agg
      //   - compute source's AgentFitness via cross_agent::fitness
      //   - filter by min_samples, min_fitness, min_source_weight, exclude_patterns
      //   - dedupe: per skill name, keep the source with highest per-agent fitness

  sort candidates by (population_fitness desc, agent name asc)
  cap at min(opts.max_per_sweep, default max_per_sweep)

  for each candidate (peer, skill, fitness, samples):
    cmd_install(home, registry_url, "agent://{peer}/{skill}",
                InstallContext::AutoPropagate { source_fitness, source_samples })
      // existing M4b path; on success it already mutates transfer_chain
      // and writes SkillStats. M7c adds: append CreditEntry{ kind: "propagator", .. }

  release lock; emit report; exit 0 (or 5 if any install failed)
```

### 6.2 Credit view

```
run_credit(home, agent, skill_name):
  // self ledger
  entries = ledger::read_for_skill(home, agent, skill_name)

  // peer ledgers (read-only)
  for peer in list_peer_agents(home).filter(p != agent):
    entries.extend(ledger::read_for_skill(home, peer.name, skill_name))

  // also fold evolution events for full mutator coverage
  for src in {agent} ∪ peers:
    for evt in evolution::read_log(home, src, skill_name):
      if evt is "evolve" and no matching mutator entry: synthesise CreditEntry
      if evt is "Recombined" and no matching recombiner entry: synthesise CreditEntry

  group + sort + render
```

The "synthesise from evolution log if missing" fallback covers history from before M7c — agents that evolved skills under M3c had no ledger; the credit view degrades gracefully.

### 6.3 Intent canonicaliser

```
canonicalise(home):
  intents = []
  for each agent dir:
    for each skill manifest:
      for each ProcedureStep:
        intents.push((step.intent, agent_name, skill_name))

  groups = group_by(normalize(intent_str))
  canonical_entries = []
  for (norm, items) in groups:
    counts = items.group_by(original_str).map(|(s, xs)| (s, xs.len()))
    canonical = counts.sort_by(count desc, alphabetical asc).first()
    aliases = counts.keys()
    canonical_entries.push({ canonical, aliases, count: items.len() })

  write <home>/intent_canonical.yaml (atomic temp + rename)
```

---

## 7. Error handling

All exit codes deterministic, no retries, no silent fallback:

| Code | Where | Condition |
|---|---|---|
| 0 | propagate / credit / intent | success |
| 2 | credit | skill not found in any ledger or evolution log |
| 2 | intent show | no canonical file exists |
| 4 | propagate | no peers on host |
| 5 | propagate | one or more sub-installs failed |
| 5 | intent canonicalise | write failed |
| 6 | propagate | candidate skill name collides with local skill but with lower fitness (refuses to overwrite) |
| 7 | propagate | lock held |

Inside an idle-hook invocation, exit codes are mapped to log levels: 0/4 → info, 5/6 → warn, 7 → debug (expected when sweeps overlap). The hook never propagates a non-zero exit to the supervisor — a failing sweep should not crash the agent.

---

## 8. Testing

### 8.1 Unit tests

- `credit.rs`: serde round-trip for all four `kind` variants; unknown-kind tolerance on read.
- `propagate/candidates.rs`: gate enforcement — separate test per gate (min_samples, min_fitness, min_source_weight, exclude_patterns), version-skip behavior, source-tiebreak determinism.
- `credit/ledger.rs`: append, read-for-skill filter, atomic-append survives partial-write scenarios (mock fs).
- `credit/aggregate.rs`: group + render; "synthesise from evolution log when ledger missing" path.
- `intent/canonical.rs`: normalize cases, group counting, tiebreak rules, idempotent rebuild (rebuilding from current file yields same file).

### 8.2 Integration tests (under `mur-core/tests/`)

1. `propagate_pull_only.rs` — synthetic 3-peer fixture, run propagate from one, assert only the invoker's home gained files; peer files unchanged (mtime, content).
2. `propagate_gates.rs` — eight scenarios covering each gate independently (samples too low, fitness too low, source weight too low, exclude pattern hit, local already newer, no peers, max_per_sweep cap, lock held).
3. `propagate_idle_hook.rs` — register `skill-propagate` via `idle-add`, fake time forward past `after_secs`, assert one sweep ran and ledger has one new propagator entry.
4. `credit_view_aggregates_peers.rs` — three peers with mixed ledger entries; `mur skill credit` output (JSON) is asserted field-by-field.
5. `credit_synthesises_from_evolution_log.rs` — empty ledger but populated evolution.log; credit view still produces mutator entries.
6. `intent_canonicaliser_e2e.rs` — three peers with overlapping intents; assert canonical file matches frequency rules; second invocation is idempotent.
7. `intent_inject_lookup.rs` — manifest has alias intent → injector emits canonical form.

### 8.3 Manual smoke

- Run `mur agent propagate --dry-run` against a real `~/.mur` with two installed agents and divergent skills. Verify report is sensible.
- Wire `skill-propagate` idle hook, leave the agent idle for 30 min, verify install + ledger append.
- Run `mur skill credit <name>` after a recombine event from M7b; verify both parents appear with `recombiner` kind.
- Run `mur skill intent canonicalise` and inspect the YAML; rebuild and verify file is byte-identical.

---

## 9. Open questions

All resolved in design. Scoping doc questions tagged for M7c:

- **Q5 (credit without currency)** — resolved: per-agent JSONL ledger, four `kind` values, no token. Fitness weight (M7a) is the incentive. Leaderboard surface is `mur skill credit` and `mur agent peers --fitness` (already in M7a).
- **Q6 (trust inheritance)** — resolved: unchanged from M4b/M7b. Skills enter at `Sandboxed Draft`. Promotion via existing maturity pipeline. No special "trusted source → trusted skill" rule in M7.
- **Q7 (intent canonicaliser ownership)** — resolved: per-host `~/.mur/intent_canonical.yaml`, any agent can write (atomic temp+rename), frequency-only clustering, embedding clustering deferred to M7+.

---

## 10. Carried out of scope

- Pushing skills to peers via A2A — deliberate; pull-side preserves invariants.
- Remote-host peer discovery (`peers.yaml`, configured remotes) — scoping §7.
- Trust elevation on inheritance (trust auto-promote based on source reputation) — scoping §7 + M4b model still correct.
- Token / currency for credit — scoping §7.
- Embedding-based intent clustering — M7+.
- Cross-agent contradiction detection (legitimate divergence ≠ bug) — M7b §12.
- Modifying `SkillManifest` for lineage — kept in sidecar `ledger.jsonl` instead.
- Per-agent credit weighting (fractional / weighted attribution) — single-axis `kind` only.

---

## 11. File-size discipline

| File | Projected lines |
|---|---|
| `credit.rs` (common) | 160 |
| `propagate/mod.rs` | 220 |
| `propagate/candidates.rs` | 200 |
| `propagate/install_ctx.rs` | 80 |
| `credit/ledger.rs` | 200 |
| `credit/aggregate.rs` | 180 |
| `intent/canonical.rs` | 240 |
| `intent/inject_lookup.rs` | 120 |
| `cmd/agent_propagate.rs` | 180 |
| `cmd/skill_credit.rs` | 200 |
| `cmd/skill_intent.rs` | 160 |

All under the 800-line ceiling. Largest is `intent/canonical.rs` at 240 lines; well inside the budget.

---

## 12. Verification checklist

Before declaring M7c complete:

1. `cargo build --workspace` clean.
2. `cargo clippy --workspace -- -D warnings` clean.
3. `cargo fmt --check` clean.
4. `cargo test --workspace` green (all new unit + integration tests above).
5. Manual smoke (per §8.3): dry-run propagate, idle-hook propagate, credit view across peers, intent canonicalise idempotent.
6. M7a invariant audit: after a full M7c sweep, no file under any `<home>/agents/<peer>/` for peer ≠ self has changed (mtime + content check).
7. `mur skill credit <recombined-skill>` after an M7b recombine shows both parents as `recombiner` and the invoker as `author`.
8. Inject path under M6b uses the canonical mapping when present (verified by `intent_inject_lookup.rs`).
9. Idle hook `skill-propagate` registered via `mur agent schedule propagate-init` exists in `~/.mur/agents/<self>/idle_triggers.yaml` and fires under the configured idle conditions.
10. Schema stability: `SkillManifest` content_hash for an existing skill is unchanged by M7c (no manifest fields added).
