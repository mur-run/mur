# Splitting `mur-common/src/agent.rs` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Follow the `rust-split-module` skill for the mechanics. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring `mur-common/src/agent.rs` (2947 lines, the largest `.rs` file in the repo) under the 800-line rule (CLAUDE.md, Mandatory Rule 5) by turning it into `mur-common/src/agent/{mod,mcp,transport,entitlements,lifecycle,companion}.rs`.

**Architecture:** This is pure code movement. Nothing is renamed, no logic changes, and no caller changes. `mod.rs` re-exports every child with `pub use child::*`, so every `mur_common::agent::X` path and the `pub use agent::{…}` block in `mur-common/src/lib.rs` keep resolving. Each test module moves with the code it tests, so no `tests/` directory is needed.

**Tech stack:** Rust (edition 2024, `mur-common`).

**Baseline:** `origin/main` @ `93b40752`. The last commit that touched the file is `951c64f6`. All line numbers below refer to that state. **If `agent.rs` has changed since then, re-derive the ranges before generating anything** (Task 1).

### Global Constraints

- One file, one PR. Behaviour cleanups spotted along the way go in a separate PR.
- `//!` inner docs (lines 1–2) stay the first lines of `mod.rs`.
- A `#[serde(default = "helper")]` helper stays in the same module as its struct, or stays reachable through `use super::*`. Serde resolves the path in the struct's module.
- Expected visibility changes: **none** (see Traps). If `cargo check` demands one, list it in the PR body with the reason.

### Child layout

| child | line ranges (baseline) | contents | lines | `#[test]` |
|---|---|---|---|---|
| `mod.rs` | 1–292, 1193–1195, 1842–1962, 2141–2248, 2616–2669, 2929–2947 | header + `use`s, `SkillCardEntry`/`SkillCardTrigger`, `AgentProfile`, Identity/Persona/Model, `default_true`, `impl AgentProfile`; `model_ref_tests`, `skill_card_tests`, `secrets_field_tests` | 597 | 8 |
| `mcp.rs` | 294–605, 2302–2418, 2875–2903, 2905–2927 | `McpServerEntry`, `McpPackagePin`, `McpAuth`, `OauthAuth`, `McpNetMode`, `ENV_MCP_DENY_HOSTS`, `McpServerNetwork`, `AddonRef`, `McpPublisherInfo`; `mcp_pin_tests`, `remote_mcp_tests`, `requires_programs_tests` | 481 | 6 |
| `transport.rs` | 607–713 | Transport / Tcp / Webhook / Noise / Socket / Auth / Communication configs and their defaults | 107 | 0 |
| `entitlements.rs` | 715–973, 2671–2856 | `Entitlements`, Network/DNS/Filesystem/Processes/Spawn/Syscalls/Limits, `AUTHORING_DIRS`, `ToolPolicy`, `ToolRule`, `resolve_tool_policy*`; `tool_policy_tests` | 445 | 11 |
| `lifecycle.rs` | 975–1191, 1197–1373, 1964–2139, 2462–2495, 2858–2873 | Notifications, Retry, Lifecycle, `ExecutionMode`, schedule consts, `ScheduleProposal`/`ScheduleEntry`, `IdleTrigger`, `name_enabled`/`set_denylist`, FileTransfer, Deployment, `DroppedGrant`, `filesystem_grants_digest`, `SandboxRecord`, `LockFile`, `LockTransports`; `tests`, `idle_trigger_tests`, `lockfile_compat_tests` | 620 | 11 |
| `companion.rs` | 1375–1840, 2250–2300, 2420–2460, 2497–2571, 2573–2614 | Voice, Hitl, `CompanionConfig`, `default_locale`/`normalize_lang`, onboarding, Proactive/Rhythm/Quiet/Active hours, `AgentAppearance`, Behavior/Render/Snapshot, `PatternFilter`, `SnapshotRef`, `FederationConfig`, `ProactiveTier` (pulled out from between the test modules); `hitl_tests`, `locale_tests`, `voice_tests`, `appearance_tests`, `federation_tests` | 675 | 23 |
| **total** | | | | **59** (same as baseline) |

Line counts are before the `mod`/`pub use`/`use super::*` header lines and before `cargo fmt`. The largest child (`companion.rs`) still has more than 100 lines of headroom.

The ranges are recorded as data in
`~/.mur/artifacts/<agent>/agent-split-plan/plan.json`. It is a run artifact, not part of the tree, so regenerate it from the table above if it is missing:

```json
{"src": "mur-common/src/agent.rs",
 "children": {
  "mod":          [[1,292],[1193,1195],[1842,1962],[2141,2248],[2616,2669],[2929,2947]],
  "mcp":          [[294,605],[2302,2418],[2875,2903],[2905,2927]],
  "transport":    [[607,713]],
  "entitlements": [[715,973],[2671,2856]],
  "lifecycle":    [[975,1191],[1197,1373],[1964,2139],[2462,2495],[2858,2873]],
  "companion":    [[1375,1840],[2250,2300],[2420,2460],[2497,2571],[2573,2614]]
 }}
```

A dry run against the baseline found: no line assigned twice, no unassigned non-blank line, every range ending on `}` or `;`, every range starting on a non-blank line, and test counts summing to 59.

### Traps already checked (baseline)

| trap | finding | handling |
|---|---|---|
| serde `default = "…"` across children | 30 distinct helpers. All but one live in the same child as their struct. `default_true` (1193) is used by `Entitlements` (735) and `IdleTrigger` (1171). | Keep `default_true` in `mod.rs` as a private fn. Both children reach it through `use super::*`, so the attribute needs no edit. |
| private `fn` used from another child | Only `default_true` (above). `normalize_lang` is used only in `companion.rs` (`default_locale` + `locale_tests`). | none |
| `use super::…` in moved tests | 13 modules use `use super::*`. `locale_tests` uses `use super::normalize_lang`. `requires_programs_tests` and `secrets_field_tests` use absolute `crate::agent::…` paths. | All still resolve, because each child starts with `use super::*` and `normalize_lang` moves with its test. No edits. |
| `use super::*` in the middle of `mod tests` (line 2038) | It is a second import, placed after the first four tests. | Moves verbatim with the module. |
| doc/attr blocks split from their item | Cut points were computed from each item's first `///`/`#[` line: `ProactiveTier` docs start at 2250, schedule consts at 1090, `DroppedGrant` at 1274, Voice section header at 1375. | Ranges above already include them. |
| private struct fields / private types | No private fields and no private `struct`/`enum`/`const`/`type` items. All 87 `pub` names are unique. | none |
| child name clashing with `crate::` paths | Existing `crate::companion` (dir) is reached via `crate::companion::{Formality, Relationship}`. Inside `agent/`, the child is `self::companion`, a different path. | Keep the absolute `use crate::companion::…` line in `mod.rs` and do not rewrite it to a relative path. |
| `macro_rules!` | none | — |

### Out-of-tree references to update (live docs only)

| file | current reference | becomes |
|---|---|---|
| `docs/architecture/mcp-supply-chain.md:158` | `mur-common/src/agent.rs` — `McpPackagePin::lockfile_path` | `mur-common/src/agent/mcp.rs` |
| `docs/platforms/freebsd-audit.md:148` | row keyed on `mur-common/src/agent.rs` + the `launchd` doc line | `mur-common/src/agent/companion.rs`. **CI gate:** `scripts/check-freebsd-audit.py` keys rows by path + line text, so a stale path fails "FreeBSD platform audit". |
| `mur-core/tests/doctor_bridges_section.rs:14` (comment) | `mur-common/src/agent.rs:691` (already stale line number) | `mur-common/src/agent/lifecycle.rs` (`profile_round_trip_yaml` in `mod tests`), with no line number |

Historical plans and specs in `docs/superpowers/plans/` and `docs/superpowers/specs/` are left alone.

---

## Task 1 — Preflight

- [x] `git switch main && git pull --ff-only`, then branch `refactor/split-common-agent`. A local branch with that name already exists, created at `93b40752`. Reuse it if `main` has not moved, or recreate it.
- [x] `git log -1 --format=%h origin/main -- mur-common/src/agent.rs`. If it is not `951c64f6`, re-derive the ranges (the `rust-split-module` §2–3 commands) and rerun the dry-run coverage check before going further.
- [x] Check for open PRs that touch the file: `gh pr list --repo mur-run/mur --state open --json number,files -q '.[] | select(.files[].path == "mur-common/src/agent.rs") | .number'`. Seven remote branches touch it, but all are 325+ commits behind `main` (newest 2026-09-19). Treat them as stale, but confirm none has an open PR.
- [x] `df -h .`: a workspace check needs several GB. 67 GiB were free at planning time.

## Task 2 — Generate the children

- [x] Run the `rust-split-module` §4 generator against `plan.json`. It asserts full coverage and writes `mur-common/src/agent/*.rs`, then deletes `agent.rs`.
- [x] Fix up `mod.rs` by hand. The `//!` lines go first, then the four original `use` lines, then `mod mcp; mod transport; mod entitlements; mod lifecycle; mod companion;` and the matching `pub use <child>::*;` lines. The generator puts the `mod` lines above the inner docs, which is a compile error.
- [x] Each child starts with `use super::*;` (generator default). The `serde`/`BTreeMap`/`ProgramDep` imports come in through that.

## Task 3 — Prove equivalence

- [x] `cargo check -p mur-common --all-targets`. Fix only visibility or import paths, and record each one.
- [x] `cargo fmt --all`
- [x] Test count: `git show HEAD:mur-common/src/agent.rs | grep -c '#\[test\]'` (59) equals `cat mur-common/src/agent/*.rs | grep -c '#\[test\]'`. Also check `cargo test -p mur-common --lib -- --list | grep -c '^agent::'`. Test paths change from `agent::<mod>::…` to `agent::<child>::<mod>::…`, and the count must match.
- [x] `cargo test -p mur-common --lib agent::`
- [x] Public surface diff is empty:
  ```bash
  diff <(git show HEAD:mur-common/src/agent.rs | grep -oE '^pub (struct|enum|const|fn|trait|type) [A-Za-z_0-9]+' | sort) \
       <(cat mur-common/src/agent/*.rs        | grep -oE '^pub (struct|enum|const|fn|trait|type) [A-Za-z_0-9]+' | sort)
  ```
- [x] `cargo clippy --all --all-targets --no-deps --locked -- -D warnings` (CI invocation, verbatim).
- [x] `cargo check --workspace --all-targets --locked`. 1068 files reference `mur_common::agent` and must compile untouched.
- [x] `wc -l mur-common/src/agent/*.rs`: every file is ≤ 800.
- [x] `python3 scripts/check-freebsd-audit.py` passes after the doc row update.

## Task 4 — References and PR

- [x] Update the three references in the table above.
- [x] `git grep -n 'mur-common/src/agent\.rs' -- ':!docs/superpowers/plans' ':!docs/superpowers/specs'` returns nothing.
- [x] Commit as `refactor(common): split agent.rs into submodules (pure code movement)`.
- [x] PR body contains: the child-layout table (file → lines → contents), the visibility-change list (expected: none), the verification table (check / clippy / fmt / test count / public-surface diff / workspace check / FreeBSD audit / `wc -l`), and the sentence "Pure code movement, no behavior change." End the body with the configured MUR attribution line.

## Out of scope

- Splitting the mixed `mod tests` (digest, sandbox record, MCP net serde, and profile round-trip tests) by topic. Moving individual test fns changes test paths beyond the module rename, so it belongs in a follow-up.
- Moving the `pub use agent::{…}` block in `lib.rs`.
- The other 81 files over 800 lines (`executor/dag.rs` 2704, `cmd/fleet/loop_run.rs` 2560, `cmd/sync_cmd.rs` 2541, `dispatch.rs` 2516, …).
