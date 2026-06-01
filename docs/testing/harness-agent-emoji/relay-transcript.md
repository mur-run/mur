
## 1. Product Manager (`tt-pm`)  —  15.9s

# PRD: mur agent emoji

## Goal
Surface agent status visually via emoji in CLI listing and A2A agent card.

## User Stories

**US1** — As a mur user running `mur agent list`, I see an emoji column so I can scan agent health at a glance.

**US2** — As an A2A client consuming an agent card, I receive an `emoji` field reflecting the agent's current status.

## Status → Emoji Mapping
| Status   | Emoji |
|----------|-------|
| running  | 🟢    |
| stopped  | ⚪    |
| error    | 🔴    |
| starting | 🟡    |

## Acceptance Criteria

1. A single source-of-truth function/method maps `AgentStatus` → `&'static str` emoji.
2. `mur agent list` output includes a new leftmost (or clearly placed) `EMOJI` column; existing columns unchanged in order/content.
3. The A2A agent card JSON includes a string field `"emoji"` populated from the same mapping.
4. All four statuses render correctly; unknown/future statuses fall back to a neutral emoji (e.g. ❔) without panicking.
5. Unit tests cover all four mappings + fallback.
6. `mur agent list` snapshot/integration test asserts emoji column presence for at least one status.
7. No breaking change to existing CLI flags or A2A schema consumers (additive only).

## Out of Scope
- Configurable/user-customizable emoji.
- Colorization, TTY detection, or `--no-emoji` flag.
- New agent statuses or status-transition logic.
- Localization, accessibility text alternatives.
- Changes to non-`list` agent subcommands (e.g. `show`, `inspect`).
- Web UI / dashboard changes beyond the A2A card field.
- Persisting emoji in storage/DB.

## Deliverable
One PR: mapping module + CLI column + A2A card field + tests + brief CHANGELOG entry.

HANDOFF -> architect: Design the smallest module layout for a shared `status_emoji` mapping consumed by both the `agent list` CLI renderer and the A2A agent card serializer, including the fallback behavior.

## 2. System Architect (`tt-arch`)  —  13.6s

# Architecture: mur agent emoji

## Crate Layout

**`mur-common`** (shared types) — owns the mapping. Both CLI and A2A serializer depend on this; no cycles.

**`mur-core`** (CLI) — consumes mapping in `agent list` renderer and `AgentCard` builder.

## File Changes

### `mur-common/src/agent_status.rs` (NEW or extend existing)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Stopped,
    Error,
    Starting,
}

impl AgentStatus {
    /// Single source of truth. Non-exhaustive-safe via fallback.
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Running  => "🟢",
            Self::Stopped  => "⚪",
            Self::Error    => "🔴",
            Self::Starting => "🟡",
        }
    }

    pub const FALLBACK_EMOJI: &'static str = "❔";
}
```
Fallback `❔` is exposed as a const for callers deserializing unknown status strings (serde error path → neutral display, no panic).

### `mur-common/src/agent_card.rs`
Extend `AgentCard` struct:
```rust
pub struct AgentCard {
    // ...existing fields (name, version, capabilities, etc.)
    pub status: AgentStatus,
    pub emoji: String,   // NEW — populated from status.emoji()
}
```
Builder/constructor sets `emoji: status.emoji().to_string()`. Additive field; existing consumers unaffected.

### `mur-core/src/cli/agent/list.rs`
Add `EMOJI` as leftmost column in the table renderer (e.g. `comfy-table` or existing writer). Read via `status.emoji()`. No flag changes.

## Public Surface
- `mur_common::AgentStatus::emoji(&self) -> &'static str`
- `mur_common::AgentStatus::FALLBACK_EMOJI`
- `AgentCard.emoji: String` (additive JSON field)

## Non-Goals Honored
No new config in agent `profile.yaml`, no changes to `Pattern`/`KnowledgeBase`, no new CLI flags.

HANDOFF -> rust-engineer: Implement `AgentStatus::emoji()` in `mur-common`, add the `emoji` field to `AgentCard`, and wire the leftmost `EMOJI` column into `mur-core`'s `agent list` renderer.

## 3. Rust Engineer (`tt-rust`)  —  11.8s

# Rust Implementation: mur agent emoji

Edition 2024. Three focused changes; all additive.

## 1. `mur-common/src/agent_status.rs`

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Stopped,
    Error,
    Starting,
}

impl AgentStatus {
    /// Single source of truth for status → emoji.
    pub const fn emoji(&self) -> &'static str {
        match self {
            Self::Running  => "🟢",
            Self::Stopped  => "⚪",
            Self::Error    => "🔴",
            Self::Starting => "🟡",
        }
    }

    /// Neutral glyph for unknown/deserialization-failure paths.
    pub const FALLBACK_EMOJI: &'static str = "❔";
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
            Self::Starting => "starting",
        })
    }
}
```

Re-export in `mur-common/src/lib.rs`:
```rust
pub mod agent_status;
pub use agent_status::AgentStatus;
```

## 2. `mur-common/src/agent_card.rs` (additive field)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCard {
    pub name: String,
    pub version: String,
    // ...existing fields...
    pub status: AgentStatus,
    /// Derived from `status`; serialized for A2A consumers that don't know the enum.
    pub emoji: String,
}

impl AgentCard {
    pub fn new(name: String, version: String, /* … */ status: AgentStatus) -> Self {
        Self {
            name,
            version,
            status,
            emoji: status.emoji().to_string(),
            // ...
        }
    }
}
```

## 3. `mur-core/src/cli/agent/list.rs` — leftmost column

```rust
use comfy_table::{Table, ContentArrangement};
use mur_common::AgentStatus;

pub fn render(agents: &[AgentRow]) -> String {
    let mut table = Table::new();
    table
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["EMOJI", "NAME", "STATUS", "PID", "UPTIME"]);

    for a in agents {
        table.add_row(vec![
            a.status.emoji().to_string(),
            a.name.clone(),
            a.status.to_string(),
            a.pid.map_or("-".into(), |p| p.to

## 4. DevOps Engineer (`tt-devops`)  —  14.1s

# DevOps: mur agent emoji

Minimal CI/build deltas. No new crates, no new features flags, no new workspace members.

## 1. CI gates (existing `.github/workflows/ci.yml` — no new jobs)

Confirm these run on the PR; no edits required if already present:

```yaml
- run: cargo fmt --all -- --check
- run: cargo clippy --workspace --all-targets --all-features -- -D warnings
- run: cargo build --workspace --locked
- run: cargo test  --workspace --locked
```

Action item: ensure `-D warnings` is in effect — the new `AgentStatus` enum and `Display` impl must land clippy-clean (watch for `clippy::use_self`, `missing_docs` if enforced on `mur-common`).

## 2. Workspace / Cargo.toml

- `mur-common`: no new deps (`serde` already present).
- `mur-core`: no new deps (`comfy_table` already present per existing `list.rs`).
- Edition 2024 already pinned at workspace root — nothing to change.

## 3. Encoding / locale

CI runners (ubuntu-latest, macos-latest, windows-latest) must emit UTF-8. Add once to the Windows job if not present:

```yaml
- run: chcp 65001
  if: runner.os == 'Windows'
```

Otherwise emoji bytes in snapshot tests may garble.

## 4. Versioning (SemVer)

- `mur-common`: **minor bump** — new public enum `AgentStatus` and new public `AgentCard.emoji` field. The added struct field is technically breaking for exhaustive struct literals; mitigate by marking `AgentCard` `#[non_exhaustive]` if not already, else treat as **major**.
- `mur-core`: **minor bump** — additive CLI column.
- A2A schema consumers: `emoji` is additive and optional-on-read; document in CHANGELOG under "Added".

## 5. Release

Single PR, single tag. No migration, no feature flag, no rollout staging needed.

HANDOFF -> code-reviewer: Review the Rust diff + these CI/version notes for correctness, API stability (especially the `AgentCard` field addition), and CHANGELOG completeness.

## 5. Code Reviewer (`tt-review`)  —  15.0s

# Code Review: mur agent emoji

I'm reviewing against the architecture + DevOps notes (the Rust diff itself wasn't included, so findings target the agreed design + the contract points DevOps flagged).

## Blocking

1. **`AgentCard` field addition is a breaking change unless gated.** DevOps correctly flagged this. Required: either `#[non_exhaustive]` on `AgentCard` *before* this PR lands, or bump `mur-common` major. Also confirm all internal constructors use `..Default::default()` or struct-update syntax so downstream rebuilds don't fail. Pick one and document in CHANGELOG.

2. **Emoji mapping must live in `mur-common`, not duplicated in `mur-core`'s `list.rs`.** Single source of truth: `impl AgentStatus { pub fn emoji(&self) -> &'static str }` (or `Display` on a wrapper). The `list` command and the A2A card serializer must both call it. Reviewer will reject any inline `match` in `list.rs`.

3. **No hardcoded emoji literals scattered in call sites.** Per mur conventions, constants belong in one module (e.g. `mur_common::agent::status`). Verify no `"🟢"` string literals appear outside that module.

## Non-blocking / nits

4. **`AgentStatus` should be `#[non_exhaustive]`** to allow future variants (e.g. `Paused`) without a major bump.
5. **Serde**: ensure `#[serde(rename_all = "lowercase")]` on `AgentStatus` so A2A JSON stays stable; add a round-trip test.
6. **`emoji` field in A2A card**: confirm `#[serde(skip_serializing_if = "Option::is_none")]` if optional, else document it as always-present.
7. **File size**: confirm `list.rs` stays ≤800 lines after the new column; if close, extract the table-row builder.
8. **CHANGELOG.md**: must list both the `AgentStatus` addition and the `AgentCard.emoji` field under "Added", and call out the `#[non_exhaustive]` decision under "Changed".

Please paste the actual diff for a line-level pass.

HANDOFF -> security: Review the emoji feature for any injection, terminal-escape, or A2A-deserialization risks (untrusted status values rendered to TTY or echoed into JSON).

## 6. Security Engineer (`tt-sec`)  —  18.8s

# Security Review: mur agent emoji

Scope is small but touches a TTY renderer and a network-exposed A2A card. Risks below.

## Risks + Mitigations

1. **Terminal escape injection — LOW (with current design).**
   Mapping is a closed `match` over an internal `AgentStatus` enum returning `&'static str` literals controlled by us. No untrusted bytes reach the terminal. **Mitigation/guardrail:** add a unit test asserting each emoji constant contains only the expected codepoint and no C0/C1 control chars (`\x1b`, `\x07`, etc.). Reject any future PR that sources the emoji from config/env/remote.

2. **A2A inbound `emoji` field — MEDIUM if deserialized from peers.**
   If `AgentCard` is ever *received* (not just served), an attacker-controlled `emoji` string could carry ANSI escapes, zero-width joiners, RTL overrides (U+202E), or oversized grapheme bombs, and later be printed by `mur agent list` on a peer. **Mitigations:**
   - Treat inbound `emoji` as advisory; **never render peer-supplied emoji to TTY**. Render only from local `AgentStatus::emoji()`.
   - If it must be displayed, validate: single grapheme cluster, ≤8 bytes, no chars in `Cc`/`Cf` categories, no `U+202A–202E`, `U+2066–2069`.
   - Add a serde `deserialize_with` that enforces the above; reject on violation.

3. **Status enum from untrusted JSON — LOW.**
   With `#[non_exhaustive]` + `#[serde(rename_all="lowercase")]`, unknown variants should fail closed. Confirm no `#[serde(other)]` fallback that silently maps to `Running`.

4. **Secret leakage — none identified.** Emoji column adds no new fields sourced from secrets/paths/tokens. Confirm `AgentCard` serialization doesn't now include status for agents the caller isn't authorized to see (authz unchanged, but worth a grep).

5. **Entitlements / sandbox — no impact.** No new FS, network, or IPC surface; pure in-process mapping + existing serializer.

## Required before merge
- Control-char test on the emoji table.
- Decision recorded: is A2A `emoji` **outbound-only**? If yes, document it; if no, add the deserialize validator.

HANDOFF -> qa: Verify tests cover (a) status→emoji mapping for all variants, (b) JSON round-trip stability, (c) no control chars in emoji constants, and (d) `mur agent list` rendering on a non-UTF8/!TTY stdout.

## 7. QA Engineer (`tt-qa`)  —  17.4s

# QA Test Plan: mur agent emoji

## Test Matrix

### Unit Tests (`agent/status.rs`)
1. **`emoji_mapping_exhaustive`** — assert each variant maps to expected glyph:
   - `Running → "🟢"`, `Stopped → "⚪"`, `Error → "🔴"`, `Starting → "🟡"`.
   - Use exhaustive `match` in test (no wildcard) so new variants force test update.
2. **`emoji_no_control_chars`** — for every variant, assert `emoji()` contains no chars in `Cc`/`Cf` categories, no `\x1b`, `\x07`, no `U+202A–202E`, `U+2066–2069`; byte len ≤ 8; exactly one grapheme cluster (via `unicode-segmentation`).
3. **`emoji_static_lifetime`** — compile-time check `fn _x() -> &'static str { AgentStatus::Running.emoji() }`.

### Serde Tests (`a2a/card.rs`)
4. **`card_serializes_emoji_field`** — serialize `AgentCard{status: Running, ..}`, assert JSON contains `"emoji":"🟢"`.
5. **`card_roundtrip_stable`** — serialize → deserialize → re-serialize; bytes equal.
6. **`unknown_status_rejected`** — `{"status":"melting"}` returns `Err`, does **not** default to Running. Confirms no `#[serde(other)]`.
7. **(Conditional)** if inbound `emoji` accepted: `inbound_emoji_with_ansi_rejected` — `"\x1b[31mX"` fails deserialize. Otherwise: `emoji_is_outbound_only` — assert field has `#[serde(skip_deserializing)]` via behavior test.

### CLI Integration Test (`tests/agent_list.rs`)
8. **`list_renders_emoji_column`** — spawn `mur agent list` against fixture with one agent per status; assert stdout contains each emoji and column header `EMOJI` (or equivalent).
9. **`list_non_tty_stdout`** — pipe stdout (`Stdio::piped()`); assert command exits 0 and emoji bytes appear (UTF-8 by default; we do not strip on non-TTY per architect spec, but verify no panic).
10. **`list_alignment`** — assert column widths computed using grapheme width, not byte len (no shifted columns).

## Acceptance Verdict

**CONDITIONAL PASS** — ship when:
- Tests 1–6, 8–10 implemented and green in CI.
- Security item resolved: documented decision on inbound `emoji` (recommend outbound-only via `#[serde(skip_deserializing)]`); test 7 reflects choice.
- `cargo test`, `clippy -D warnings`, and the existing `agent list` snapshot updated.

No blocking defects identified in design. Scope remains single-PR shippable.

HANDOFF -> pm: QA con

---
## Handoff-quality summary

| # | Role | chars | handoff? | latency |
|---|------|-------|----------|---------|
| 1 | Product Manager | 1874 | YES | 15.9s |
| 2 | System Architect | 2033 | YES | 13.6s |
| 3 | Rust Engineer | 2398 | NO | 11.8s |
| 4 | DevOps Engineer | 1875 | YES | 14.1s |
| 5 | Code Reviewer | 2034 | YES | 15.0s |
| 6 | Security Engineer | 2277 | YES | 18.8s |
| 7 | QA Engineer | 2254 | YES | 17.4s |
