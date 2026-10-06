# Agent file editing — edits as events, files as views

- **Status:** Draft rev 2. Rev 1 (journal-only) was superseded before commit: it treated
  editing as a tool feature; this revision treats it as an **event source** that feeds
  the workspace substrate, the signed channel, and the memory pipeline MUR already has.
- **Research input:** deep-research run of 2026-10-06
  (`~/.mur/artifacts/deep-research/20261006-055037-state-of-the-art-for-ai-coding-agent-file-editin.md`,
  7 claims, all CONFIRMED). The industry baseline is summarised in §1; this design
  deliberately goes past it (§2, §9).
- **Scope:** `edit_file` / `write_file` in `mur-agent-runtime`, the settlement card, the
  per-agent channel, the daemon, and the `capture → store → retrieve → inject` /
  `evolve` memory pipeline in `mur-core`.
- **Non-scope:** the model-facing edit *syntax* (D1 keeps it), fleet partition/merge
  semantics (reused, not changed), and the Hub GUI (consumer only, P3).

## 1. What the research established (industry baseline)

| # | Finding | Source (verified) | Consequence for MUR |
|---|---------|-------------------|---------------------|
| 1 | Aider ships `editblock` (search/replace) and `udiff`, picked per model. | aider `coders/*_prompts.py` | Search/replace is first-class at the top of the field. |
| 2 | Aider's udiff is `diff -U0`: zero context, hunks anchored by content. | aider udiff prompts | Context-line fragility is why udiff needs per-model tuning. |
| 3 | Codex CLI `apply_patch` has a formal grammar (`*** Begin Patch` … `@@`). | codex `apply-patch` parser | Grammar-backed formats are heavy to adopt and model-specific. |
| 4 | Anthropic `str_replace_based_edit_tool` takes `old_str`/`new_str`, unique match required. | anthropic-sdk tool types | MUR's `edit_file` is already this shape. |
| 5 | Codex isolates runs by `environment_id`, not filesystem snapshots. | codex grammar | Nobody surveyed uses CoW snapshots for edit truth — the gap MUR fills. |
| 6 | Aider auto-commits every applied edit; undo = `git undo`. | aider `base_prompts.py` | Undo must not assume a repo. |
| 7 | Tool definitions support `cache_control` / `defer_loading`. | anthropic-sdk | Orthogonal; noted so nobody re-researches it. |

Desktop Commander's `edit_block` adds the one practical pattern a model needs on a
miss: closest-substring search, a similarity score, and a character-level
`{-old-}{+new+}` diff between expected and closest text (§6).

Every system surveyed shares the same ceiling: the edit tool is the **only** source of
truth about what changed, undo is **per tool call**, and no edit survives as
**knowledge** past the session. MUR already owns the parts that lift that ceiling.

## 2. Existing MUR mechanisms this design composes (nothing new invented)

| Mechanism | Where | Reused for |
|-----------|-------|------------|
| `ParallelBackend` trait — `create_track` / `base_snapshot` / `diff_files` / `promote` / `destroy`; backends ZFS native, ZFS-over-socket (Lima/OrbStack), git worktree; APFS/Btrfs `cp -c`/`--reflink` clone helper | `mur-core/src/parallel/backend/` | Per-turn CoW tracks (§3) |
| Daemon-side ZFS protocol (`ZfsRequest` / `ZfsResponse`) and `SnapshotRequest` drop files — sandboxed runtime asks, daemon acts | `mur-common/src/zfs_protocol.rs`, `snapshot_request.rs`, `mur-daemon/src/snapshot_requests.rs` | Runtime never shells out to `zfs` (§3.3) |
| Signed, append-only channel; `load_events_with_damage`; fleet review rollback math | `mur-channel/src/store.rs`, `mur-core/src/cmd/fleet/review/rollback.rs` | Edit ledger (§4) |
| `action_hash`-matched HITL gate; deferred (never timed-out) unattended approvals | fleet safety triad | Approving exact bytes (§4.4) |
| tree-sitter semantic units with content-addressed identity; region partition; N-way hunk merge | `mur-core/src/parallel/{semantic,partition,concurrent}` | Promoting byte ranges to semantic provenance (§5.1) |
| Four-stage memory pipeline; `evolve/telemetry_reader` already consumes `telemetry/tool_call` | `mur-core/src/{capture,store,retrieve,inject,evolve}` | Edit provenance as memory (§5) |
| `VersionedYamlStore` — `~/.mur` is a git repo, one commit per save | `mur-core/src/store/versioned/` | Memory writes join the same ledger (§5.4) |
| `redact_secrets` | `mur-common/src/redact.rs` | Ledger payload hygiene (§4.2) |

## 3. Decisions

- **D1 — Exact-literal search/replace stays the only edit primitive.** `old_string` /
  `new_string` / `expected_count` unchanged. No regex mode, no udiff, no apply_patch.
- **D2 — Fuzzy matching is diagnostic only.** It shapes the error message (§6); the file
  is modified exclusively on an exact match.
- **D3 — The settlement card reports what the disk says, not what the tool says.**
  `~ changed` comes from `diff_files(track, base)`, so `sed -i`, `cargo fmt`, and script
  side effects are visible. Tool-reported edits are a *subset* annotation, never the
  source.
- **D4 — An edit is a signed, content-addressed channel event; the file is a view.**
  `edit.applied` / `edit.reverted` carry hashes, not bytes; bytes live in a
  per-agent CAS (§4.2).
- **D5 — Three undo scopes, three actors.** `undo_edit` (model, current turn, one
  event); `mur agent turn undo <agent> <turn>` (human, whole turn = destroy track);
  `mur agent edits undo <agent> <event-id>` (human, any event, by inverse apply).
- **D6 — Optimistic concurrency on `before_hash`, no locks.** A fleet member whose
  `before_hash` no longer matches the disk gets a rejection that names the other
  agent's `intent` (§4.3).
- **D7 — Provenance is a memory tier, not a log.** Edit events enter `capture`;
  `retrieve` surfaces them on the units a tool is about to touch, inside the existing
  5-pattern / ~2000-token budget (§5.2).
- **D8 — Backends move below `mur-core`.** `mur-agent-runtime` must not depend on
  `mur-core` (LanceDB/Arrow); the backend trait and implementations are extracted into
  a new crate `mur-track` (§3.3), per the "shared state gets its own crate" rule.

## 4. Layer 1 — Substrate: every writing turn runs on a CoW track

### 4.1 Turn lifecycle

1. First write-capable tool call of a turn → `create_track("turn-<seq>")`, then
   `base_snapshot`. Read-only turns never create a track (zero cost for chat).
2. Tool writes go to the track path; the agent's cwd is rebound for the turn.
3. Turn end → `diff_files(track, base)` → settlement `~ changed`. Then, by policy:
   - **direct** (default, interactive): `promote(track, project)`; track destroyed.
   - **shadow** (fleet unattended, opt-in `edits.shadow: true` in `limits:`): verify
     commands run *on the track*; promote only on green, else keep the track for
     review and emit `settlement.blocked` with the failing command.
4. `mur agent turn undo` = `destroy(track)` before promote, or inverse-apply the turn's
   events after promote (§4).

### 4.2 Backend selection (reuses `detect_backend`, order unchanged)

| Host | Substrate | Notes |
|------|-----------|-------|
| Linux/FreeBSD on ZFS, `zfs` CLI | ZFS clone per turn | True volume snapshot; `diff_files` = `zfs diff`. |
| macOS with Lima/OrbStack ZFS socket | ZFS over socket | Daemon owns the socket; runtime drops `SnapshotRequest` files. |
| macOS APFS (no VM) | **per-file clonefile shadow** | Volume snapshots (`tmutil`) are Time Machine-bound and read-only when mounted — not usable. Shadow = `cp -c` of each file before first write; `diff_files` = hash compare against shadow set + mtime walk for files the tools never touched. |
| anything else | git worktree | Always available; `diff_files` = `git status --porcelain` in the worktree. |

### 4.3 Crate extraction (`mur-track`)

Pure code movement of `mur-core/src/parallel/backend/{mod,detect,zfs_native,zfs_socket,git_worktree,cow}.rs`
plus `mur-common/src/{zfs_protocol,snapshot_request}.rs` into `mur-track`, depending on
std + anyhow + serde only. `mur-core` re-exports; no behaviour change in that PR.
Runtime gains the dependency; sandbox policy allows the per-agent track dir
(`~/.mur/agents/<name>/tracks/`) and nothing more.

## 5. Layer 2 — Ledger: `edit.applied` is a channel event

### 5.1 Event shape

```yaml
kind: edit.applied            # or edit.reverted
seq: 4182                     # channel sequence, signed Ed25519 like every event
agent: worker_2
turn: 17
path: <project-relative>      # e.g. the file holding fn validate
range: { start: 1180, end: 1312 }   # byte offsets in the *before* file
before_hash: sha256:…         # whole file, before
after_hash:  sha256:…         # whole file, after
old: sha256:…                 # CAS ref; inline when ≤ edits.inline_bytes (default 4 KiB)
new: sha256:…
intent: "rotate refresh token before expiry check"   # model-supplied, ≤ 200 chars
cause: { user_msg: 9f3a…, tool_call: 3 }             # provenance back-pointers
```

`write_file` emits one event with `range` covering the whole file. Bash side effects
detected by §4.1 step 3 emit `edit.observed` (no `old`/`new` CAS, hashes only) so the
ledger stays complete even when the tool was not MUR's.

### 5.2 Storage

- Events: the agent's existing channel (append-only, signed, damage-aware load).
- Bytes: `~/.mur/agents/<name>/edits/cas/<sha256>` — immutable, deduplicated,
  `redact_secrets` applied to the inline copy *and* refused as a CAS write if the
  redactor fires (the event is still recorded with `cas: redacted`).
- Retention: CAS objects unreferenced by any event younger than `edits.retain_days`
  (default 30) are pruned by the daemon; events are never pruned (channel semantics).
- Hashing uses `sha2`, already a runtime dependency; no new hash crate.

### 5.3 Optimistic concurrency (fleet)

Before applying, the tool hashes the current file. If it differs from the model's
`before_hash` (captured at the last `read_file`), the edit is **rejected** with:

```
edit_file rejected: <path> changed since you read it.
  by worker_1, 40 s ago, intent: "add clock-skew tolerance to expiry check"
  re-read the file; your old_string may still match.
```

No locks, no partition required for small overlaps. Large planned overlaps keep using
`mur fleet partition-plan` — the ledger merely makes unplanned ones safe and *legible*.

### 5.4 HITL

Above-`read` tier edits park on the existing gate; the approval is matched on
`action_hash` = hash(`path`, `before_hash`, `old`, `new`). An approved event replays
byte-exactly or not at all — the unattended-defer rule needs no change.

## 6. Layer 3 — Memory: edit provenance enters the pipeline

### 6.1 Semantic lift (daemon, outside the sandbox)

The runtime stays light (no tree-sitter). The daemon tails `edit.*` events and, for
grammars it has (Rust today), maps `range` → semantic unit id via
`parallel/semantic` (`group_by_identity`). Other languages keep byte-range provenance.
Result is written as `edit.lifted` (unit id, kind, name) back to the channel.

### 6.2 Retrieve / inject

When `read_file` or `edit_file` targets a file or unit with ledger history, `retrieve`
adds at most one provenance pattern, ranked with the others inside the existing
budget:

```
provenance <path>::fn validate
  3 edits in 7 d · 2 reverted · last intent: "rotate refresh token before expiry check"
  reverted because: settlement.blocked — cargo test auth::expiry (worker_2, turn 17)
```

Cross-agent by construction: the channel is shared, so `git blame` with *reasons*, for
every agent in the fleet, without a repo.

### 6.3 Evolve

New signals for `evolve`, all derived from the ledger, none requiring model calls:

- **revert cluster** — same unit reverted ≥ 2× → candidate rule ("do not change X
  without Y"), surfaced via the existing pattern-proposal path.
- **near-miss streak** — `edit_file` misses (§7) on the same file within a turn → feeds
  the doom-loop detector in `mur-agent-runtime/src/task_runner/agentic_loop.rs`.
- **change coupling** — units edited together in ≥ N turns → proactive hint when one
  is touched alone.

### 6.4 One primitive for code and memory

Skill / pattern writes (`remember`, `skill_evolve`, `VersionedYamlStore::save`) emit
the same `edit.applied` event on the `~/.mur` repo. Undo, audit trail, and HITL for
memory changes are then the *same* mechanism as for source — one ledger, one
`mur agent edits` view, one `turn undo`.

## 7. `edit_file` failure feedback (unchanged from rev 1, condensed)

On `old_string not found`: find the closest window by Myers diff length, report
`similarity` (0–1), and render `{-expected-}{+found+}` for the best window; if
similarity < `edits.suggest_threshold` (default 0.6) say so and suggest `read_file`
with an `offset` near the best line. On ambiguous match: list every match line, remind
about `expected_count`. Never apply. One dependency (Myers) serves both scoring and
rendering.

## 8. Tools, CLI, config

- **Tools:** `undo_edit { event?: id }` — reverts the given event, default last, current
  turn only, same action tier as `edit_file`.
- **CLI:** `mur agent edits {list|show|undo} <agent> [--turn N] [--path P]`;
  `mur agent turn undo <agent> <turn>`; `mur fleet edits` = same across the channel,
  grouped by unit, conflict-aware.
- **Config (`~/.mur/config.yaml`, `edits:`):** `inline_bytes`, `retain_days`,
  `suggest_threshold`, `shadow` (fleet `limits:` scope), `substrate: auto|worktree`
  (force the fallback for debugging). No value is hardcoded in the tools.

## 9. What this adds beyond the surveyed systems

1. **Truthful settlement** — backed by a snapshot diff, so a tool cannot under-report.
2. **Lock-free multi-agent editing** whose conflict message carries the other agent's
   intent.
3. **Edit history as retrievable memory** — an agent does not retry the change that
   was reverted last week, and it learns coupling from its own ledger.
4. **Shadow verify-then-promote** for unattended fleets: green tests gate the disk.
5. **Code and memory edits are one primitive** with one undo and one audit trail.

## 10. Deliberately not built (and why)

- **Unified diff / V4A `apply_patch`.** Changes how the model *speaks*, not what MUR
  *knows*; multi-file atomicity already comes from the turn track. Revisit only with
  measured per-model evidence.
- **Fuzzy apply.** D2.
- **Journal-only undo (rev 1).** Kept as the degraded mode *inside* the worktree
  backend (no CoW available), not as the design — it cannot see bash side effects and
  has no cross-agent semantics.
- **Auto-commit per edit (Aider).** Needs a repo, pollutes history; the ledger is
  repo-independent.
- **Whole-volume APFS snapshots on macOS.** `tmutil` snapshots are Time Machine-bound
  and read-only mounted; the per-file clonefile shadow gives the same per-turn truth.
- **tree-sitter in the runtime.** Keeps every agent process light; the daemon lifts.

## 11. Phasing

| Phase | Deliverable | Risk | Visible result |
|-------|-------------|------|----------------|
| P0 | `mur-track` extraction (pure move); turn tracks; settlement from `diff_files`; `mur agent turn undo`; `edit_file`/`write_file` switch to temp + rename | low — no model-facing change | honest `~ changed`, whole-turn undo |
| P1 | `edit.applied/observed/reverted` events + CAS; `undo_edit`; `before_hash` rejection; `mur agent edits` | medium — new channel kinds | per-edit undo, legible fleet conflicts |
| P2 | daemon semantic lift; provenance in `retrieve`; evolve signals (revert cluster, near-miss streak, coupling); §7 feedback | medium — pipeline touch | agents stop repeating reverted edits |
| P3 | shadow verify-then-promote; memory writes on the ledger; Hub timeline view | medium — fleet policy | unattended fleets gated by green |

Each phase is its own PR set; P0 ships with the worktree fallback exercised in CI so
every platform has a substrate from day one.

## 12. Open questions

- Q1: Does `edit.observed` need a HITL tier? Proposal: no — it records, it does not act.
- Q2: `before_hash` source — last `read_file` of the *same* turn only, or any prior
  read? Proposal: same turn; stale reads across turns must re-read.
- Q3: CAS placement for fleets — per agent (default) or one per channel? Proposal: per
  agent, hashes are global so dedup is free when the Hub merges views.
- Q4: Should `shadow` be the default for *all* fleet autorun once `MUR_FLEET_AUTORUN=1`?
  Proposal: yes, after P3 proves out; it strengthens the safety triad, never weakens it.

## 13. Acceptance criteria

- A turn whose only change came from `bash sed -i` shows that file in `~ changed`.
- `mur agent turn undo` restores every byte of the turn on all four substrates
  (ZFS native, ZFS socket, APFS shadow, worktree), covered by one shared test suite.
- Two fleet members editing the same file without a partition plan: the second gets
  the rejection message naming the first's intent; no bytes are lost.
- `undo_edit` on a `write_file` of a new file deletes it; on an existing file restores
  the CAS `old`.
- An approved HITL edit replays byte-exact after an agent restart; a drifted file
  re-parks instead of applying.
- With a unit reverted twice in the ledger, `retrieve` injects the provenance pattern
  on the next `edit_file` of that unit, within the token budget.
- Memory writes (`remember`) appear in `mur agent edits list` and can be undone there.
- `cargo clippy --all --all-targets --no-deps --locked -- -D warnings` and
  `cargo fmt --all -- --check` pass; no source file crosses 800 lines.
