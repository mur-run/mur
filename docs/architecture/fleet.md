# mur fleet — usage reference

A **fleet** is a named squad of MUR agents working a shared goal over one signed channel
(`fleet-<name>`). Thin object + a `~/.mur/fleets/<name>/fleet.yaml`, not a subsystem — it reuses
channels (blackboard), `channel/delegate` (supervisor edge), the DAG executor, skills, and HITL.

`fleet` (AI agent squad) ≠ `team` (your human org / seats) ≠ `fleet_sync` (device sync).

## A fleet's "type" = three orthogonal dimensions

### 1. Distribution — `parallel.mode` in fleet.yaml

| mode | how work is split | reconcile | orchestrator topology |
|------|-------------------|-----------|-----------------------|
| **(none)** plain | goal broadcast to all members (or a router-planned DAG); each does the full task | — | a plain squad |
| **speculative** | N tracks, **same goal, different `approach`** per track | judge scores per semantic unit → `cherry_pick` best track → `assemble_file` | **compete** (best-of-N) |
| **partition** | split **one `target_file`** into disjoint regions (LPT bin-pack), one region per track | `mur fleet merge` splices regions back | **coupled-write, disjoint** |

`explore` (read/search) is **not** a fleet — use the `parallel_jobs` MCP tool (no worktree).

### 2. Execution — how it runs

- `mur fleet run <name> [job]` — one iteration.
- `mur fleet run <name> --loop [--max-iterations N] [--deadline 2h] [--budget-usd X]` — guarded
  loop: stops on iteration cap / deadline / budget / stuck-detection, or converges on
  `done_when: marker:<TEXT>` (a member emits the marker as an own-line sentinel),
  `done_when: queue-empty` (stops once an iteration finds nothing queued), or router DONE/CONTINUE.
- **daemon auto-run** — `fleet_tick` fires any fleet whose `loop.trigger` is due (`interval:<dur>`
  or `cron:<5-field>`). Gated: `MUR_FLEET_AUTORUN=1` **and** a positive `loop.budget_usd`.
  Kill-switch: `mur fleet stop <name>` (`.stopped` sentinel); `mur fleet start <name>` clears it.

### 3. Isolation — orthogonal flag

- `MUR_PARALLEL_EXEC=1 mur fleet run …` — **Tier 1**: each track gets its own git worktree under
  `.worktrees/` (`create_tracks`), agents are prompt-routed there (bash `cwd`), fan-out capped
  (`max_concurrency`), collision guard on the main checkout. Without the flag, members share the
  checkout.

So any fleet is: **mode** (plain / speculative / partition) × **run** (once / `--loop` / auto) ×
**isolation** (worktree on/off).

## Reconcilers (the recombination half)

- **speculative** → `mur fleet compare <name>` (judge + cherry-pick best per unit)
- **partition** → `mur fleet merge <name>` (deterministic disjoint splice)
- **concurrent** (experimental, default OFF) → `MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent
  <name> [--stats] [--promote]` — zero-dep N-way line merge: disjoint hunks auto-merge, overlaps
  **escalate** (never silent-interleave); `--promote` refuses on unresolved overlap, reverts on
  `cargo check` fail. (Spike-1 measured 0.1% real overlap → the Loro CRDT was shelved; the
  zero-dep `StructuralMerger` is the final engine.)

## CLI surface

```
mur fleet create <name> --members <a> <b> …      # writes fleet.yaml + the shared channel
mur fleet list | show <name>
mur fleet run <name> [job] [--loop …]
mur fleet stop <name> | start <name>             # kill-switch
mur fleet merge <name> | compare <name>          # reconcile (partition / speculative)
mur fleet partition-plan <name>                  # preview the LPT region assignment
mur fleet export <name> [--with-members] | import <file>   # signed .fleet bundle
```

## How it composes with agents

The **orchestrator** agent decides the topology and proposes the fleet shape; **rustsmith**
(or any coder) is the member that does the writing; **mur** (concierge) is the front door.

- coherence-bound → **no fleet**, one rustsmith, single writer.
- compete → a **speculative** fleet of heterogeneous members → `compare`.
- disjoint coupled-write → a **partition** fleet → `merge`.
- explore → **no fleet** (read-only `parallel_jobs`).

Members need **repo-root write** to write in their worktrees; budget + kill-switch + HITL are
external to the agents (daemon). See `docs/superpowers/specs/2026-06-19-mur-fleet-design.md` for
the design.
