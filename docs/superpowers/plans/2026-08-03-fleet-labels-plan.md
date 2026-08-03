# Fleet labels — implementation plan

**Spec:** `docs/superpowers/specs/2026-08-03-fleet-labels-design.md`
**Execution skill:** `mur-executing-plans` (in-context, sequential)

## Goal

Give the MUR Hub's fleet list a taxonomy: a central label registry at
`~/.mur/labels.yaml`, many-to-many fleet↔label assignments, and a rail that
both filters by label chips (OR) and groups fleets under their primary label
(first entry of the ordered assignment list), each fleet appearing exactly once.

## Architecture

`mur-common::labels` owns the pure types + validation. `mur-core::cmd::fleet::labels`
owns the atomic YAML store (tmp+rename, mirroring `store::save_fleet`). The Hub's
`src-tauri/src/fleet.rs` exposes five new Tauri commands and adds `labels` to
`FleetSummary`. The React rail's bucketing/filtering is a *pure* module
(`fleetLabels.ts`) so it is unit-testable with the repo's existing vitest setup
(no jsdom in this project — never test through the DOM here).

## Tech stack

Rust (serde_yaml, anyhow, tempfile for tests) · React 18 + TypeScript · vitest.

## Global Constraints

- Central registry only; `fleet.yaml` is never modified by this feature.
- Primary label = `assignments[fleet][0]`; no separate `primary:` field.
- Unknown label ids are dropped on load; missing/corrupt file = empty registry, non-fatal.
- `valid_label_id`: `[a-z0-9-_]`, non-empty, ≤32 — refuses traversal.
- A fleet appears in exactly one group.
- Empty chip selection = All; multi-select is OR; composes AND with the search query.

## File structure

| File | Responsibility |
| --- | --- |
| `mur-common/src/labels.rs` (new) | `Label`, `LabelRegistry`, `valid_label_id`, primary/normalize logic |
| `mur-common/src/lib.rs` | register `pub mod labels;` |
| `mur-core/src/cmd/fleet/labels.rs` (new) | load/save/set/create/rename/delete/prune on disk |
| `mur-core/src/cmd/fleet/mod.rs` | register `pub mod labels;` |
| `mur-hub-gui/src-tauri/src/fleet.rs` | `labels` on `FleetSummary`; 5 commands |
| `mur-hub-gui/src-tauri/src/lib.rs` | register the commands |
| `mur-hub-gui/ui/src/components/fleet/fleetLabels.ts` (new) | pure filter+group |
| `…/fleetLabels.test.ts` (new) | vitest for the above |
| `…/types.ts` | `LabelView`, `labels` on `FleetSummary` |
| `…/FleetRail.tsx` | chip row + grouped list |
| `…/FleetView.tsx` | load labels, own chip selection (localStorage) |
| `…/FleetDetail.tsx` | label editor (toggle, make primary) |
| `…/i18n/{en,zh-TW}.ts` | new keys |
| `…/styles.css` | chip + group heading styles |

## Tasks

1. `mur-common::labels` — types, `valid_label_id`, `normalize`, `primary_of`. Tests first.
2. `mur-core::cmd::fleet::labels` — store roundtrip, unknown-id drop, delete scrubs, prune. Tests first.
3. Hub commands + `FleetSummary.labels`.
4. `fleetLabels.ts` pure logic + vitest.
5. Rail chips + groups; FleetView wiring + persistence.
6. FleetDetail label editor; i18n; CSS.
