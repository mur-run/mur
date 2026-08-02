# Fleet labels — grouping the Hub's fleet list

**Date:** 2026-08-03
**Status:** design proposed, awaiting approval
**Scope:** MUR Hub `Fleets` tab (rail + registry). CLI is out of scope for v1.

## Problem

The Hub's fleet rail is one flat list of every fleet
(`mur-hub-gui/ui/src/components/fleet/FleetRail.tsx:29` maps `fleets` directly).
Past ~8 fleets it stops answering "which of these is the web work?" — the user's
actual request was *classification*, and labels were only the first idea they
reached for.

## Decisions taken (grilling log)

| Question | Chosen |
| --- | --- |
| Taxonomy shape | **Labels, many-to-many** (not a single-parent folder tree) |
| Rail presentation | **Chips as filters _and_ group headings** — both |
| Duplicate listing | **No** — a fleet's **primary label** decides its group; it appears once |
| Storage | **Central registry**, not scattered in each `fleet.yaml` |

Rejected alternatives, briefly: a folder tree (single parent, forces a false
choice for a fleet that is both `rust` and `infra`); free-text tags with no
registry (no rename, no colour, typo-forks the taxonomy); labels living in
`fleet.yaml` (rename = N file rewrites, and no place to store label order).

## Data model

New file `~/.mur/labels.yaml`, one central registry:

```yaml
labels:
  - id: web            # slug: [a-z0-9-_], <=32, unique
    display: Web
    color: "#4a9eff"   # optional; rail chip tint
  - id: rust
    display: Rust
assignments:
  develop-web: [web, rust]   # fleet name -> label ids, ORDER IS MEANINGFUL
  deep-research: [research]
```

- **Primary label = `assignments[fleet][0]`.** No separate `primary:` field: one
  ordered list cannot disagree with itself. Reordering in the UI re-groups.
- A fleet absent from `assignments`, or with `[]`, is **Ungrouped**.
- Registry is the only writer of taxonomy; `fleet.yaml` is untouched, so
  `fleet export/import` and existing fleets keep working with no migration.
- Unknown label ids in `assignments` are dropped on load (self-healing after a
  hand-edit), and a fleet name with no fleet on disk is pruned on save.

## Components

**`mur-common/src/labels.rs`** — `Label { id, display, color }`,
`LabelRegistry { labels, assignments }`, `valid_label_id()` mirroring
`valid_fleet_name()` (`mur-common/src/fleet.rs:69`).

**`mur-core/src/cmd/fleet/labels.rs`** — store, same atomic tmp+rename shape as
`store::save_fleet` (`mur-core/src/cmd/fleet/store.rs:26`):
`load(home)`, `save(home, &reg)`, `set_labels(home, fleet, ids)`,
`create_label`, `rename_label`, `delete_label` (removes it from every
assignment), `prune(home)`.

**`mur-hub-gui/src-tauri/src/fleet.rs`** — `FleetSummary` gains
`labels: Vec<String>` (ids, primary first), populated in `fleet_list()`.
New commands: `fleet_labels_list` → `Vec<LabelView { id, display, color,
fleet_count }>`, `fleet_label_set { name, labels }`, `fleet_label_create`,
`fleet_label_rename`, `fleet_label_delete`. Registered in `lib.rs:704`.

**`FleetRail.tsx`** — chip row above the list; then grouped `<ul>`s.

**`FleetDetail.tsx`** — a label editor (add/remove chips, drag or "make
primary") so labels are assignable without leaving the Hub.

## Rail behaviour

1. **Chips**: `All` + one chip per label (with `fleet_count`) + `Ungrouped`.
   Multi-select, **OR** semantics: a fleet shows if _any_ of its labels is
   selected. Empty selection = All.
2. **Groups**: visible fleets are bucketed by **primary label** and rendered
   under a sticky group heading, in registry order; `Ungrouped` last.
3. A group with zero visible fleets is hidden entirely.
4. Selection filter composes with the existing `query` search
   (`FleetView.tsx:20`) — AND.
5. Selected chips persist to `localStorage` per-window; a fleet that the filter
   hides while selected stays selected (its detail pane does not blank).

## Error handling

- Missing/corrupt `labels.yaml` → treated as empty registry, non-fatal; the rail
  degrades to today's flat list plus an `Ungrouped` heading.
- Duplicate label id on create → rejected with a toast, existing chip flashes.
- Delete label → confirm, then removed from all assignments; affected fleets
  fall back to their next label, or to Ungrouped.

## Testing

Rust: registry roundtrip; unknown-id drop on load; `delete_label` scrubs
assignments; `prune` drops dead fleet names; `valid_label_id` refuses traversal
(`../evil`) exactly as `save_fleet_refuses_traversal_name` does.
UI: primary-label bucketing puts a two-label fleet in exactly one group; OR
filter across chips; empty-group hiding; chip + search compose.

## Out of scope (v1)

CLI `mur fleet label`, label-scoped bulk actions (stop all `web`), colours in
`murmur`, and syncing labels to MUR Server teams.

## Decision (approved 2026-08-03)

Primary = **first entry in the ordered list**, edited by reordering; no separate
`primary:` field — fewer states, cannot desync. Spec final.
