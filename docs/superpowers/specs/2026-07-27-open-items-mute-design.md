# Open Items: muting noisy sources

**Date:** 2026-07-27
**Status:** design, approved in brainstorm
**Scope:** `mur open` display policy. Agent-side injection of open items is explicitly out of scope and gets its own spec.

## Problem

`mur open` shows what is outstanding, split into `observed` (derived from MUR's own state) and `reported` (an agent said so). It has no way to stop showing something the user has already decided about.

On the machine this was designed against, one line reads:

```
246 harvested workflow proposals awaiting review         [inbox]
```

Those proposals have accumulated since 2026-06-12 at roughly eight a day. Whatever the user does about them, they are not a decision that needs re-making at the bottom of every `mur open` and after every agent turn.

The cost is not the line itself. A status surface spends the reader's willingness to look, and a line representing a settled decision spends it for nothing — displacing the two lines that are worth reading (a fleet stopped by its kill-switch, a job queued behind it). The failure mode of a status surface is not being wrong once; it is being ignored from then on.

### Note on the specific case

Investigation during the brainstorm found that 221 of those 246 proposals have their command arguments replaced by `<STR>` / `<PATH>` / `<N>`, because the harvest pipeline writes its *matching skeleton* — a normalization built to compare procedures across sessions — into the proposal's `steps` field. A step reading `grep -rn <STR> --include=<STR> .` cannot be run or reviewed.

**That is a separate defect, filed as #777.** This spec does not depend on it: a mute mechanism is worth having whether or not any given source is producing good output, and building one only for sources known to be broken would mean rebuilding it for the next noisy source.

## Design

### Mute collapses; it never hides

Muting a source removes its lines and adds one line at the end of the list:

```
● observed — from MUR's own state
  fleet 'rust-solo' is stopped by its kill-switch          [fleet:rust-solo]
      → mur fleet start rust-solo

2 sources muted (inbox, fleet:old) — mur open --all
```

This single rule is what makes a permanent mute safe. The reader never has to wonder whether something is hidden, because the answer is always on screen. The real trade is not *show* versus *hide* — it is **one line versus N lines**, and one line is spent no matter how many sources are muted.

### Mute is permanent until reversed, by design

Muted sources do not come back on a timer and do not come back on a change threshold.

- **A timer** (`snooze 7d`) models *not now*. The 246 proposals are not a deferral, they are a standing decision, and re-asking a settled question every seven days is worse than asking it once.
- **A change threshold** ("come back if it doubles") is unpredictable in a way that defeats the purpose: the user mutes something, it later reappears, and the explanation is a rule they never chose and cannot see. A mute that silently un-mutes is worse than one that never does, because then neither the surface nor the mute can be relied on. It also degenerates in practice — at eight new proposals a day any count threshold is crossed within a week.

The objection to permanence is "what if that source later holds something important?" That is an argument about **granularity**, not about timers: if a source contains both noise and signal, the mute is being applied at the wrong level. `origin` is already fine-grained (`fleet:acme` is not `fleet:other`), and a source that genuinely mixes both should be split at the collector, not papered over with an unpredictable resurrection rule.

`until` can be added to the stored form later without breaking anything, if real usage shows a need. It is not in v1.

### Mute is per `origin`, which already exists

`OpenItem.origin` is already `"inbox"`, `"fleet:<name>"`, `"agent:<name>"`. Muting is a set of origin strings. No new taxonomy.

Matching is **exact**, not prefix. `fleet:acme` mutes that fleet and nothing else; there is no way to mute every fleet at once in v1. Prefix matching would make `fleet` — a plausible thing to type — silently swallow every fleet in the list, which is the one outcome a mute must never produce by accident. If muting a whole family turns out to be a real need, it should arrive as explicit syntax rather than as an emergent property of string prefixes.

### Mute state lives in `config.yaml`, not the log

```yaml
open_items:
  muted:
    - inbox
    - fleet:old
```

Not in `open-items.jsonl`. That log is append-only and **agent-writable** via the `open_item` tool. Muting is a user decision, and an agent must not be able to overturn it by appending a record. This mirrors `fleet_run.agents`, which lives in the global config for the same reason.

## Components

| Where | Change |
|---|---|
| `mur-common/src/config.rs` | `OpenItemsConfig { muted: Vec<String> }` on `Config`, `#[serde(default)]` |
| `mur-core/src/open_items/mod.rs` | `partition(items, muted) -> (visible, muted_origins)`; `render` gains the footer; `fingerprint` computed over visible only |
| `mur-core/src/cli/actions.rs` | `OpenAction::{Mute, Unmute}` |
| `mur-core/src/dispatch.rs` | `--all` flag; wire mute/unmute through config load → save |
| `mur-core/src/cmd/agent/cli/app.rs` | turn summary uses the visible set |

`collect()` is unchanged and still returns everything. Policy is applied above it, so the collectors stay ignorant of display rules and `--json` can report both halves.

## Data flow

```
config.yaml ─┐
             ├─→ partition() ─→ visible ──→ render / summary_line / fingerprint
collect() ───┘                └─→ muted_origins ──→ footer line
```

`--json` emits the full item list plus a top-level `"muted": ["inbox"]`. Consumers (Hub) get everything and apply their own policy; nothing is dropped from the machine-readable form.

**This changes the `--json` shape** from a bare array to `{ "items": [...], "muted": [...] }`. `mur open --json` shipped hours ago in #771 and has no known consumer yet, so the break is taken now rather than carried forever as a second output mode. The alternative — leaving the array and losing the mute state — would mean Hub could not tell an empty list from a fully muted one.

## Error handling

**On error, show rather than hide.** An unreadable or malformed `config.yaml` yields an empty mute set, so every item appears. Hiding on failure is the dangerous direction: it produces a quiet, confident, incomplete list.

`mur open mute <origin>` records the origin even when no current item carries it — a source can be legitimately empty today — but prints a warning naming the origins currently in use, so a typo is visible immediately rather than at the next silent no-op.

`unmute` of something not muted is not an error. The requested end state holds.

## Testing

| Test | Guards |
|---|---|
| muted origin's items do not appear in `visible` | the basic mechanism |
| footer names every muted source, and only appears when something is muted | mute is never invisible |
| `--all` returns everything and prints no footer | the escape hatch |
| `fingerprint` ignores muted items | muting a noisy source silences the turn notice too, which is where it matters most |
| a muted source that changes does not wake the summary | permanence is real, not cosmetic |
| unreadable config yields no mutes, all items visible | fail toward showing |
| `mute` on an unused origin records it and warns | typos surface at the point of the mistake |
| `--json` carries muted items plus the `muted` key | policy does not truncate the machine-readable form |

## Out of scope

- **Agent-side injection** of open items into session-start context. Deferred by decision in the brainstorm: injecting a source this noisy would waste context and mislead the agent, so noise control comes first and injection is designed once real usage shows which signals matter.
- **Snooze / time-boxed mute.** See above.
- **Fixing the harvest proposal generator.** Filed as #777.
- **Per-item mute.** `mur open done` already clears a reported item; observed items clear themselves when the underlying state resolves.
