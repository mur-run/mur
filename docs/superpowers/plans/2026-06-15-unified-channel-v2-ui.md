# Unified Channel v2 — Hub "Work" View + CLI TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the v2 UI layer on top of the v1 Channel store — a Hub "Work" view (channel list / event feed / participants+trace) and a channel-aware CLI TUI — rendering only what v1 actually produces, with forward-compatible handling so v3 event kinds (delegation, tool calls, HITL, artifacts) slot in later without rework.

**Architecture:**
- **Hub backend** (`mur-hub-gui/src-tauri/src/work.rs`): three read-only Tauri commands (`channel_list`, `channel_events`, `channel_get`) that fold `mur_channel::ChannelService` + manifests into frontend DTOs. Logic lives in pure helpers taking `home: &Path` (unit-testable against a tempdir, exactly like the existing `chat::persist_exchange`); thin `#[tauri::command]` wrappers call them with `mur_home_path()`. Live refresh reuses the **existing** `channel-updated` watcher already wired in `lib.rs:419-432`.
- **Hub frontend** (`mur-hub-gui/ui`): an "Agents | Work" surface toggle in the dashboard bar; a `WorkView` three-pane component (list / feed / trace). All decision logic is extracted into pure, vitest-tested functions in `work/format.ts`; components stay thin. A forward-compatible `ChannelEventItem` switches on event `kind` and renders unknown/v3 kinds as a labeled card rather than crashing or mislabeling. The Work view is **read-only** in v2 (observability inbox) — two-way chat stays in the Agents view.
- **CLI** (`mur-core/src/cmd/agent/cli`): surface the current channel (short id + lifecycle state) in the status bar; add `/channels` to list and switch channels; `murmur a1 a2 a3` multiplex panes inherit the channel state via the shared status bar (no multiplex code change).

**Tech Stack:** Rust (`mur-channel` workspace member; `mur-core` ratatui TUI; `mur-hub-gui` Tauri 2 backend — **workspace-excluded**), React 18 + TypeScript + Vite + vitest (Hub UI).

**Scope guardrails (from the v2 decision):**
- Render only v1-produced events: `Message` (human/agent) and `Note` (system/shell). Everything else is forward-compatible scaffolding only — **no producers are added** (those are v3).
- No new A2A surface, no orchestration, no `channel/delegate`. Pure UI over the existing store.
- Work view is read-only (no compose box, no agent dial).

**Key facts locked during exploration (do not re-derive):**
- `mur-hub-gui` is in `Cargo.toml`'s `exclude` list → Hub-backend tests/clippy/fmt use `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`, and `cargo fmt` must be run **separately** for it (CI runs fmt 3×; the workspace fmt skips excluded crates).
- `mur-channel` and `mur-core` are workspace members.
- `ChannelService` public API (from `mur-channel/src/service.rs`): `open(&Path)`, `create_for_agent(&str)`, `append_message(id, actor, kind, text, task_id)`, `load_events(id)`, `list(limit) -> Vec<ChannelRow{id,title,state,updated_at}>`, `latest_for_agent(&str)`, `store()`, `index()`. `store().load_manifest(id) -> Channel`.
- `Channel` (mur-common/src/channel.rs): `{ v, id, title, goal{statement, acceptance_criteria}, state, owner, participants[{actor, role, joined_at}], created_at, updated_at }`. `ChannelActor` is `#[serde(tag="kind", rename_all="kebab-case")]` → `{kind:"human",name}` / `{kind:"agent",id}` / `{kind:"system"}`. `ChannelEvent` = `{ seq, ts, actor, kind, payload, idempotency_key }`. `EventKind` + `ChannelState` + `ParticipantRole` serialize kebab/lowercase.
- The watcher in `lib.rs` already does `handle.emit("channel-updated", channel_id)` — the v2 Work view subscribes to it; no new watcher needed.
- CLI persistence is already channel-backed (`mur-core/src/cmd/agent/cli/persist.rs`): `Session` lazily creates the channel on first append and holds `channel_id: Option<String>`; `Session::channel_id()` exists; `persist::{load, list_recent, latest}` operate on channels.

---

## File Structure

**Created:**
- `mur-hub-gui/src-tauri/src/work.rs` — read-only channel query commands + pure helpers + DTOs.
- `mur-hub-gui/ui/src/work/types.ts` — TS mirrors of `ChannelSummary`, `Channel`, `ChannelEvent`, `ChannelActor`, `Participant`.
- `mur-hub-gui/ui/src/work/format.ts` — pure formatting/decision helpers (state badge, event variant + label, relative time, actor name, preview).
- `mur-hub-gui/ui/src/work/format.test.ts` — vitest unit tests for `format.ts`.
- `mur-hub-gui/ui/src/components/work/WorkView.tsx` — three-pane composer + data loading + live refresh.
- `mur-hub-gui/ui/src/components/work/WorkChannelList.tsx` — left rail (channel list + state badges).
- `mur-hub-gui/ui/src/components/work/WorkFeed.tsx` — center event stream.
- `mur-hub-gui/ui/src/components/work/ChannelEventItem.tsx` — forward-compatible single-event renderer.
- `mur-hub-gui/ui/src/components/work/WorkTrace.tsx` — right pane (participants + goal/state + v3 trace slot).
- `mur-hub-gui/ui/src/styles/components/work.css` — Work view styles.

**Modified:**
- `mur-hub-gui/src-tauri/src/lib.rs` — declare `pub mod work;` and register the 3 commands in `invoke_handler`.
- `mur-hub-gui/ui/src/components/DashboardApp.tsx` — `surface` toggle + conditional Work render.
- `mur-hub-gui/ui/src/styles/index.css` (or wherever component CSS is imported) — import `work.css`.
- `mur-hub-gui/ui/src/i18n/en.ts` + `mur-hub-gui/ui/src/i18n/zh-TW.ts` — `work.*` keys.
- `mur-core/src/cmd/agent/cli/persist.rs` — `ChannelMeta` + `Session::current()`.
- `mur-core/src/cmd/agent/cli/app.rs` — `App.channel` cache, `refresh_channel`, `switch_channel`, `SlashCmd::Channels`, `SLASH_COMMANDS`, `parse_slash`.
- `mur-core/src/cmd/agent/cli/ui.rs` — status-bar channel segment.
- `mur-core/src/cmd/agent/cli/mod.rs` — `/channels` handling in `handle_slash`, `HELP` string.

---

## Phase A — Hub backend: channel query commands

### Task A1: `work.rs` DTOs + pure `summary_of` mapper (with test)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/work.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-hub-gui/src-tauri/src/work.rs` with the DTOs, the pure mapper, and a unit test:

```rust
//! Read-only channel queries for the Hub "Work" view (Unified Channel v2).
//!
//! The Work view is observability over the shared `~/.mur/channels/` store: it
//! lists every channel, shows one channel's event stream, and surfaces goal /
//! participants / state. It writes nothing — two-way chat stays in `chat.rs`.
//!
//! Command logic lives in pure helpers taking `home: &Path` so it is unit-tested
//! against a tempdir (mirrors `chat::persist_exchange`); the `#[tauri::command]`
//! wrappers are thin shims that pass `mur_home_path()`.

use mur_channel::ChannelService;
use mur_common::channel::{Channel, ChannelActor, ChannelEvent};
use serde::Serialize;
use std::path::Path;

/// A channel participant flattened for the frontend.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct WorkParticipant {
    /// "human" | "agent" | "system".
    pub kind: String,
    /// Agent id or human name ("" for system).
    pub id: String,
    /// "owner" | "router" | "delegate" | "observer".
    pub role: String,
}

/// One row in the Work left rail. Folds the manifest + a cheap event scan.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct ChannelSummary {
    pub id: String,
    pub title: String,
    /// kebab-case `ChannelState`.
    pub state: String,
    /// `goal.statement` (may be empty).
    pub goal: String,
    pub created_at: String,
    pub updated_at: String,
    pub participants: Vec<WorkParticipant>,
    /// Convenience: just the agent participant ids, for the rail's avatars.
    pub agents: Vec<String>,
    /// Event count (turns).
    pub turns: usize,
    /// First human message, truncated — the rail's subtitle.
    pub preview: String,
}

/// Serialize a `ChannelState`/role/actor enum to its kebab/lowercase string.
fn enum_str<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

fn participant_of(actor: &ChannelActor, role_str: String) -> WorkParticipant {
    let (kind, id) = match actor {
        ChannelActor::Human { name } => ("human", name.clone()),
        ChannelActor::Agent { id } => ("agent", id.clone()),
        ChannelActor::System => ("system", String::new()),
    };
    WorkParticipant {
        kind: kind.to_string(),
        id,
        role: role_str,
    }
}

/// Pure: build a rail summary from a manifest + its events. No I/O.
pub fn summary_of(ch: &Channel, events: &[ChannelEvent]) -> ChannelSummary {
    let participants: Vec<WorkParticipant> = ch
        .participants
        .iter()
        .map(|p| participant_of(&p.actor, enum_str(&p.role)))
        .collect();
    let agents: Vec<String> = participants
        .iter()
        .filter(|p| p.kind == "agent")
        .map(|p| p.id.clone())
        .collect();
    let preview = events
        .iter()
        .find(|e| matches!(e.actor, ChannelActor::Human { .. }))
        .and_then(|e| e.payload.get("text").and_then(|v| v.as_str()))
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    ChannelSummary {
        id: ch.id.clone(),
        title: ch.title.clone(),
        state: enum_str(&ch.state),
        goal: ch.goal.statement.clone(),
        created_at: ch.created_at.to_rfc3339(),
        updated_at: ch.updated_at.to_rfc3339(),
        participants,
        agents,
        turns: events.len(),
        preview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::EventKind;
    use tempfile::TempDir;

    #[test]
    fn summary_of_extracts_agents_preview_and_state() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("qa").unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "find the bug",
            None,
        )
        .unwrap();
        let manifest = svc.store().load_manifest(&ch.id).unwrap();
        let events = svc.load_events(&ch.id).unwrap();

        let s = summary_of(&manifest, &events);
        assert_eq!(s.id, ch.id);
        assert_eq!(s.agents, vec!["qa".to_string()]);
        assert_eq!(s.preview, "find the bug");
        assert_eq!(s.turns, 1);
        // v1 freezes state at its initial value; just assert it round-trips kebab.
        assert!(!s.state.is_empty() && !s.state.contains('"'));
    }
}
```

- [ ] **Step 2: Run test to verify it fails (does not compile / module not wired)**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml work::tests::summary_of_extracts_agents_preview_and_state`
Expected: FAIL — `work.rs` is not yet declared as a module (`error[E0583]: file not found for module` is fixed in A4, but the test target won't resolve). This confirms the module needs wiring. (If you prefer the test to compile now, do Step from A4 first; either order is fine — they commit together logically.)

- [ ] **Step 3: (no new impl needed)** — the code above is the implementation. Proceed to A4 to wire the module, then re-run.

- [ ] **Step 4: Commit** (after A4 makes it compile + pass)

```bash
git add mur-hub-gui/src-tauri/src/work.rs
git commit -m "feat(hub-work): ChannelSummary DTO + pure summary_of mapper

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task A2: query helpers `list_channels` / `events_for` / `manifest_for` (with test)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/work.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `work.rs`:

```rust
    #[test]
    fn list_channels_folds_manifests_and_skips_empty() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        // One real channel with a turn…
        let a = svc.create_for_agent("qa").unwrap();
        svc.append_message(&a.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();
        // …and one empty stub that must be filtered out of the rail.
        let _empty = svc.create_for_agent("ghost").unwrap();

        let rows = list_channels(tmp.path()).unwrap();
        assert_eq!(rows.len(), 1, "empty channels are hidden from the rail");
        assert_eq!(rows[0].id, a.id);
        assert_eq!(rows[0].agents, vec!["qa".to_string()]);

        // events_for + manifest_for hit the same channel.
        let evs = events_for(tmp.path(), &a.id).unwrap();
        assert_eq!(evs.len(), 1);
        let m = manifest_for(tmp.path(), &a.id).unwrap();
        assert_eq!(m.id, a.id);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml work::tests::list_channels_folds_manifests_and_skips_empty`
Expected: FAIL — `cannot find function list_channels/events_for/manifest_for in this scope`.

- [ ] **Step 3: Write minimal implementation**

Add to `work.rs` (above the `#[cfg(test)]` module):

```rust
/// Max channels surfaced in the rail. v2 scale is small; bump when the index
/// grows a participant column (v1-accepted follow-up).
const WORK_LIST_LIMIT: usize = 200;

/// List channels newest-first for the Work rail. Folds each manifest with a
/// cheap event scan; empty channels (created-but-never-written stubs) are
/// hidden so the rail never shows blank rows.
pub fn list_channels(home: &Path) -> anyhow::Result<Vec<ChannelSummary>> {
    let svc = ChannelService::open(home)?;
    let mut out = Vec::new();
    for row in svc.list(WORK_LIST_LIMIT)? {
        let events = svc.load_events(&row.id).unwrap_or_default();
        if events.is_empty() {
            continue;
        }
        let Ok(manifest) = svc.store().load_manifest(&row.id) else {
            continue;
        };
        out.push(summary_of(&manifest, &events));
    }
    Ok(out)
}

/// All events for one channel (the feed).
pub fn events_for(home: &Path, id: &str) -> anyhow::Result<Vec<ChannelEvent>> {
    let svc = ChannelService::open(home)?;
    Ok(svc.load_events(id)?)
}

/// One channel manifest (the trace pane: goal / participants / state).
pub fn manifest_for(home: &Path, id: &str) -> anyhow::Result<Channel> {
    let svc = ChannelService::open(home)?;
    svc.store().load_manifest(id)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml work::tests::list_channels_folds_manifests_and_skips_empty`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/work.rs
git commit -m "feat(hub-work): list_channels/events_for/manifest_for query helpers

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task A3: `#[tauri::command]` wrappers

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/work.rs`

- [ ] **Step 1: Add the command wrappers**

Append to `work.rs` (after the helpers, before `#[cfg(test)]`):

```rust
/// Tauri: list all channels for the Work rail.
#[tauri::command]
pub async fn channel_list() -> Result<Vec<ChannelSummary>, String> {
    let home = crate::mur_home_path();
    tokio::task::spawn_blocking(move || list_channels(&home))
        .await
        .map_err(|e| format!("channel_list task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Tauri: events for one channel (the feed).
#[tauri::command]
pub async fn channel_events(channel_id: String) -> Result<Vec<ChannelEvent>, String> {
    let home = crate::mur_home_path();
    tokio::task::spawn_blocking(move || events_for(&home, &channel_id))
        .await
        .map_err(|e| format!("channel_events task panicked: {e}"))?
        .map_err(|e| e.to_string())
}

/// Tauri: one channel manifest (the trace pane).
#[tauri::command]
pub async fn channel_get(channel_id: String) -> Result<Channel, String> {
    let home = crate::mur_home_path();
    tokio::task::spawn_blocking(move || manifest_for(&home, &channel_id))
        .await
        .map_err(|e| format!("channel_get task panicked: {e}"))?
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Verify it compiles (commands are not directly unit-tested — the helpers are)**

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: compiles once A4 declares the module (do A4 next, then build).

- [ ] **Step 3: Commit** (together with A4)

### Task A4: register the module + commands in `lib.rs`

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:8-24` (module declarations) and `:436-442` (`invoke_handler`)

- [ ] **Step 1: Declare the module**

After `lib.rs:24` (`pub mod seed_mur;`) add:

```rust
pub mod work;
```

- [ ] **Step 2: Register the commands**

In the `tauri::generate_handler![ … ]` list, immediately after `chat::channel_load,` (line 442) add:

```rust
            work::channel_list,
            work::channel_events,
            work::channel_get,
```

- [ ] **Step 3: Build + run the work tests**

Run:
```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml work::
```
Expected: builds; both `work::tests::*` PASS.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/work.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub-work): register channel_list/channel_events/channel_get commands

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase B — Hub frontend: the Work view

### Task B1: TS types

**Files:**
- Create: `mur-hub-gui/ui/src/work/types.ts`

- [ ] **Step 1: Write the types** (no test — pure declarations consumed by B2's tested code)

```ts
//! TS mirrors of the Rust channel DTOs (mur-common::channel + work.rs).
//! `actor` is internally tagged on `kind` (kebab-case).

export type ChannelActor =
  | { kind: "human"; name?: string }
  | { kind: "agent"; id?: string }
  | { kind: "system" };

export interface WorkParticipant {
  kind: "human" | "agent" | "system";
  id: string;
  role: string; // owner | router | delegate | observer
}

export interface ChannelSummary {
  id: string;
  title: string;
  state: string; // kebab-case ChannelState
  goal: string;
  created_at: string;
  updated_at: string;
  participants: WorkParticipant[];
  agents: string[];
  turns: number;
  preview: string;
}

export interface ChannelEvent {
  seq: number;
  ts: string;
  actor: ChannelActor;
  kind: string; // message | note | state-change | delegation | tool-call | …
  payload: { text?: string; [k: string]: unknown };
  idempotency_key?: string | null;
}

export interface Participant {
  actor: ChannelActor;
  role: string;
  joined_at: string;
}

export interface Channel {
  v: number;
  id: string;
  title: string;
  goal: { statement: string; acceptance_criteria: string[] };
  state: string;
  owner: ChannelActor;
  participants: Participant[];
  created_at: string;
  updated_at: string;
}
```

- [ ] **Step 2: Commit** (with B2)

### Task B2: pure formatting helpers + vitest tests

**Files:**
- Create: `mur-hub-gui/ui/src/work/format.ts`
- Create: `mur-hub-gui/ui/src/work/format.test.ts`

- [ ] **Step 1: Write the failing tests**

`mur-hub-gui/ui/src/work/format.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { stateBadge, eventVariant, eventKindLabel, actorName, relativeTime } from "./format";
import type { ChannelActor } from "./types";

describe("stateBadge", () => {
  it("maps known states to a class + i18n key", () => {
    expect(stateBadge("working")).toEqual({ cls: "work-badge work-badge--working", key: "work.state.working" });
    expect(stateBadge("completed")).toEqual({ cls: "work-badge work-badge--completed", key: "work.state.completed" });
  });
  it("falls back for unknown states without throwing", () => {
    const b = stateBadge("some-future-state");
    expect(b.cls).toBe("work-badge work-badge--unknown");
    expect(b.key).toBe("work.state.unknown");
  });
});

describe("eventVariant", () => {
  it("maps v1 + v3 kinds to a render variant", () => {
    expect(eventVariant("message")).toBe("message");
    expect(eventVariant("note")).toBe("note");
    expect(eventVariant("state-change")).toBe("state");
    // v3 kinds with no producer yet → forward-compatible card.
    expect(eventVariant("delegation")).toBe("card");
    expect(eventVariant("tool-call")).toBe("card");
    expect(eventVariant("hitl-request")).toBe("card");
    expect(eventVariant("totally-unknown")).toBe("card");
  });
});

describe("eventKindLabel", () => {
  it("title-cases a kebab kind", () => {
    expect(eventKindLabel("tool-call")).toBe("Tool Call");
    expect(eventKindLabel("hitl-request")).toBe("Hitl Request");
  });
});

describe("actorName", () => {
  it("uses display name from the agents map when available", () => {
    const a: ChannelActor = { kind: "agent", id: "qa" };
    expect(actorName(a, { qa: "QA Bot" })).toBe("QA Bot");
    expect(actorName(a, {})).toBe("qa");
  });
  it("labels human + system", () => {
    expect(actorName({ kind: "human", name: "alan" }, {})).toBe("alan");
    expect(actorName({ kind: "human" }, {})).toBe("you");
    expect(actorName({ kind: "system" }, {})).toBe("system");
  });
});

describe("relativeTime", () => {
  it("formats deltas coarsely", () => {
    const now = Date.parse("2026-06-15T12:00:00Z");
    expect(relativeTime("2026-06-15T11:59:30Z", now)).toBe("just now");
    expect(relativeTime("2026-06-15T11:55:00Z", now)).toBe("5m ago");
    expect(relativeTime("2026-06-15T09:00:00Z", now)).toBe("3h ago");
    expect(relativeTime("2026-06-13T12:00:00Z", now)).toBe("2d ago");
  });
});
```

- [ ] **Step 2: Run to verify it fails**

Run: `cd mur-hub-gui/ui && npm test -- format`
Expected: FAIL — `Cannot find module './format'`.

- [ ] **Step 3: Write minimal implementation**

`mur-hub-gui/ui/src/work/format.ts`:

```ts
//! Pure, framework-free formatting/decision helpers for the Work view.
//! All branching that needs testing lives here so the React components stay thin.

import type { ChannelActor } from "./types";

const KNOWN_STATES = [
  "submitted",
  "working",
  "input-required",
  "completed",
  "failed",
  "canceled",
  "rejected",
  "stale",
] as const;

/** State → { css class, i18n key }. Unknown/future states fall back safely. */
export function stateBadge(state: string): { cls: string; key: string } {
  const known = (KNOWN_STATES as readonly string[]).includes(state);
  const slug = known ? state : "unknown";
  // i18n keys use the un-hyphenated tail (e.g. input-required → inputRequired).
  const keyTail = slug.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
  return { cls: `work-badge work-badge--${slug}`, key: `work.state.${keyTail}` };
}

export type EventVariant = "message" | "note" | "state" | "card";

/** Decide how to render an event by its kind. v1 produces message/note only;
 *  every other (v3) kind renders as a forward-compatible labeled card. */
export function eventVariant(kind: string): EventVariant {
  switch (kind) {
    case "message":
      return "message";
    case "note":
      return "note";
    case "state-change":
      return "state";
    default:
      return "card";
  }
}

/** "tool-call" → "Tool Call" for the card header. */
export function eventKindLabel(kind: string): string {
  return kind
    .split("-")
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** Human-readable author for an actor, preferring the Hub's display-name map. */
export function actorName(actor: ChannelActor, displayNames: Record<string, string>): string {
  switch (actor.kind) {
    case "agent":
      return (actor.id && displayNames[actor.id]) || actor.id || "agent";
    case "human":
      return actor.name || "you";
    case "system":
      return "system";
  }
}

/** Coarse relative time. `nowMs` is injected so it is deterministic/testable. */
export function relativeTime(iso: string, nowMs: number): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const s = Math.max(0, Math.round((nowMs - then) / 1000));
  if (s < 60) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  return `${d}d ago`;
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cd mur-hub-gui/ui && npm test -- format`
Expected: PASS (all describe blocks green).

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/work/types.ts mur-hub-gui/ui/src/work/format.ts mur-hub-gui/ui/src/work/format.test.ts
git commit -m "feat(hub-work): pure work-view formatters + types (vitest)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task B3: `ChannelEventItem` (forward-compatible event renderer)

**Files:**
- Create: `mur-hub-gui/ui/src/components/work/ChannelEventItem.tsx`

- [ ] **Step 1: Implement the component** (verified by `tsc` build + B2's `eventVariant`/`actorName` tests; React render is operator-verified)

```tsx
import type { ChannelEvent } from "../../work/types";
import { eventVariant, eventKindLabel, actorName } from "../../work/format";

interface Props {
  event: ChannelEvent;
  displayNames: Record<string, string>;
}

/** Render one channel event. v1 kinds (message/note) render richly; every
 *  other kind renders as a labeled card so v3 events (delegation, tool calls,
 *  HITL, artifacts) appear without a code change and without mislabeling. */
export function ChannelEventItem({ event, displayNames }: Props) {
  const variant = eventVariant(event.kind);
  const who = actorName(event.actor, displayNames);
  const text = typeof event.payload?.text === "string" ? event.payload.text : "";

  if (variant === "message") {
    const role =
      event.actor.kind === "human" ? "user" : event.actor.kind === "agent" ? "agent" : "system";
    return (
      <div className={`work-event work-event--msg work-event--${role}`}>
        <div className="work-event__author">{who}</div>
        <div className="work-event__body">{text}</div>
      </div>
    );
  }

  if (variant === "note") {
    return (
      <div className="work-event work-event--note">
        <span className="work-event__note-text">{text || eventKindLabel(event.kind)}</span>
      </div>
    );
  }

  if (variant === "state") {
    const to = typeof event.payload?.to === "string" ? event.payload.to : text;
    return (
      <div className="work-event work-event--state">
        <span className="work-event__chip">{to || "state changed"}</span>
      </div>
    );
  }

  // Forward-compatible card for all v3 kinds. Shows the kind, the author, and a
  // compact JSON of the payload so nothing is silently dropped.
  return (
    <div className="work-event work-event--card">
      <div className="work-event__card-head">
        <span className="work-event__kind">{eventKindLabel(event.kind)}</span>
        <span className="work-event__author">{who}</span>
      </div>
      {text ? (
        <div className="work-event__body">{text}</div>
      ) : (
        <pre className="work-event__payload">{JSON.stringify(event.payload, null, 2)}</pre>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Commit** (with B4/B5 after build passes)

### Task B4: `WorkChannelList`, `WorkFeed`, `WorkTrace`

**Files:**
- Create: `mur-hub-gui/ui/src/components/work/WorkChannelList.tsx`
- Create: `mur-hub-gui/ui/src/components/work/WorkFeed.tsx`
- Create: `mur-hub-gui/ui/src/components/work/WorkTrace.tsx`

- [ ] **Step 1: WorkChannelList (left rail)**

```tsx
import type { ChannelSummary } from "../../work/types";
import { stateBadge, relativeTime } from "../../work/format";
import { useT } from "../../i18n";

interface Props {
  channels: ChannelSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  nowMs: number;
}

export function WorkChannelList({ channels, selectedId, onSelect, nowMs }: Props) {
  const { t } = useT();
  if (channels.length === 0) {
    return <div className="work-list work-list--empty">{t("work.empty")}</div>;
  }
  return (
    <div className="work-list">
      {channels.map((c) => {
        const badge = stateBadge(c.state);
        const title = c.title || c.preview || c.agents.join(", ") || c.id.slice(0, 8);
        return (
          <div
            key={c.id}
            role="button"
            tabIndex={0}
            className={`work-list__item${selectedId === c.id ? " is-active" : ""}`}
            onClick={() => onSelect(c.id)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                onSelect(c.id);
              }
            }}
            title={title}
          >
            <div className="work-list__top">
              <span className="work-list__title">{title}</span>
              <span className={badge.cls}>{t(badge.key)}</span>
            </div>
            <div className="work-list__sub">
              <span className="work-list__agents">{c.agents.join(", ") || "—"}</span>
              <span className="work-list__time">{relativeTime(c.updated_at, nowMs)}</span>
            </div>
          </div>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 2: WorkFeed (center)**

```tsx
import type { ChannelEvent } from "../../work/types";
import { ChannelEventItem } from "./ChannelEventItem";
import { useT } from "../../i18n";

interface Props {
  events: ChannelEvent[];
  displayNames: Record<string, string>;
  hasSelection: boolean;
}

export function WorkFeed({ events, displayNames, hasSelection }: Props) {
  const { t } = useT();
  if (!hasSelection) {
    return <div className="work-feed work-feed--empty">{t("work.pickChannel")}</div>;
  }
  return (
    <div className="work-feed">
      {events.map((e) => (
        <ChannelEventItem key={e.seq} event={e} displayNames={displayNames} />
      ))}
      {events.length === 0 && <div className="work-feed--empty">{t("work.noEvents")}</div>}
    </div>
  );
}
```

- [ ] **Step 3: WorkTrace (right)**

```tsx
import type { Channel } from "../../work/types";
import { stateBadge, actorName } from "../../work/format";
import { useT } from "../../i18n";

interface Props {
  channel: Channel | null;
  displayNames: Record<string, string>;
}

export function WorkTrace({ channel, displayNames }: Props) {
  const { t } = useT();
  if (!channel) return <div className="work-trace work-trace--empty" />;
  const badge = stateBadge(channel.state);
  return (
    <div className="work-trace">
      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.state")}</div>
        <span className={badge.cls}>{t(badge.key)}</span>
      </div>

      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.goal")}</div>
        <div className="work-trace__goal">{channel.goal.statement || t("work.noGoal")}</div>
        {channel.goal.acceptance_criteria.length > 0 && (
          <ul className="work-trace__criteria">
            {channel.goal.acceptance_criteria.map((c, i) => (
              <li key={i}>{c}</li>
            ))}
          </ul>
        )}
      </div>

      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.participants")}</div>
        <ul className="work-trace__participants">
          {channel.participants.map((p, i) => (
            <li key={i}>
              <span className="work-trace__pname">{actorName(p.actor, displayNames)}</span>
              <span className="work-trace__prole">{p.role}</span>
            </li>
          ))}
        </ul>
      </div>

      {/* Forward-compatible slot: v3 plan/delegation progress renders here. */}
      <div className="work-trace__section work-trace__plan" />
    </div>
  );
}
```

- [ ] **Step 4: Commit** (with B5 after build passes)

### Task B5: `WorkView` (data loading, selection, live refresh)

**Files:**
- Create: `mur-hub-gui/ui/src/components/work/WorkView.tsx`

- [ ] **Step 1: Implement WorkView**

```tsx
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useAgents } from "../../context/AgentContext";
import type { Channel, ChannelEvent, ChannelSummary } from "../../work/types";
import { WorkChannelList } from "./WorkChannelList";
import { WorkFeed } from "./WorkFeed";
import { WorkTrace } from "./WorkTrace";

/** The Work view: a read-only observability surface over every channel in the
 *  shared store. List (left) / event feed (center) / goal+participants+trace
 *  (right). Live-refreshes off the `channel-updated` watcher already emitted by
 *  the backend (lib.rs). Two-way chat stays in the Agents view. */
export function WorkView() {
  const { agents } = useAgents();
  const [channels, setChannels] = useState<ChannelSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [events, setEvents] = useState<ChannelEvent[]>([]);
  const [manifest, setManifest] = useState<Channel | null>(null);
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  const selectedRef = useRef<string | null>(null);

  // Map agent name → display name for author labels.
  const displayNames: Record<string, string> = {};
  for (const a of agents) displayNames[a.name] = a.display_name ?? a.name;

  useEffect(() => {
    selectedRef.current = selectedId;
  }, [selectedId]);

  async function loadList() {
    try {
      const rows = await invoke<ChannelSummary[]>("channel_list");
      setChannels(rows);
      setNowMs(Date.now());
      // Auto-select the newest channel on first load.
      if (selectedRef.current === null && rows.length > 0) {
        setSelectedId(rows[0].id);
      }
    } catch {
      /* best-effort: an unreadable store just leaves the rail empty */
    }
  }

  async function loadSelected(id: string) {
    try {
      const [evs, mf] = await Promise.all([
        invoke<ChannelEvent[]>("channel_events", { channelId: id }),
        invoke<Channel>("channel_get", { channelId: id }),
      ]);
      if (selectedRef.current !== id) return; // selection moved while loading
      setEvents(evs);
      setManifest(mf);
    } catch {
      setEvents([]);
      setManifest(null);
    }
  }

  // Initial load + live refresh on any channel change.
  useEffect(() => {
    void loadList();
    const un = listen<string>("channel-updated", (e) => {
      void loadList();
      if (selectedRef.current && e.payload === selectedRef.current) {
        void loadSelected(selectedRef.current);
      }
    });
    return () => {
      void un.then((f) => f());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Load the feed/trace whenever the selection changes.
  useEffect(() => {
    if (selectedId) void loadSelected(selectedId);
    else {
      setEvents([]);
      setManifest(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  return (
    <div className="work-view">
      <WorkChannelList
        channels={channels}
        selectedId={selectedId}
        onSelect={setSelectedId}
        nowMs={nowMs}
      />
      <WorkFeed events={events} displayNames={displayNames} hasSelection={selectedId !== null} />
      <WorkTrace channel={manifest} displayNames={displayNames} />
    </div>
  );
}
```

- [ ] **Step 2: Build (typecheck) to verify B3–B5 compile**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: `tsc -b` passes, `vite build` succeeds. (`useAgents`/`AgentEntry.display_name` already exist — confirm the field name matches `types.ts`; if it is `displayName`, adjust the two references.)

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/components/work/
git commit -m "feat(hub-work): WorkView three-pane (list/feed/trace) + event item

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task B6: surface toggle in DashboardApp + CSS + i18n

**Files:**
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx`
- Create: `mur-hub-gui/ui/src/styles/components/work.css`
- Modify: the CSS barrel that imports component styles (find with grep below)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`, `mur-hub-gui/ui/src/i18n/zh-TW.ts`

- [ ] **Step 1: Add i18n keys**

In `en.ts`, after the `chat.suggest.2` line (en.ts:60), add:

```ts
  "work.toggle.agents": "Agents",
  "work.toggle.work": "Work",
  "work.empty": "No channels yet — start a conversation in the Agents view.",
  "work.pickChannel": "Select a channel to see its activity.",
  "work.noEvents": "No events in this channel yet.",
  "work.state": "State",
  "work.goal": "Goal",
  "work.noGoal": "No goal set.",
  "work.participants": "Participants",
  "work.state.submitted": "submitted",
  "work.state.working": "working",
  "work.state.inputRequired": "needs input",
  "work.state.completed": "completed",
  "work.state.failed": "failed",
  "work.state.canceled": "canceled",
  "work.state.rejected": "rejected",
  "work.state.stale": "stale",
  "work.state.unknown": "unknown",
```

In `zh-TW.ts`, after its `chat.suggest.2` line, add the same keys with translations:

```ts
  "work.toggle.agents": "代理",
  "work.toggle.work": "工作",
  "work.empty": "尚無頻道 — 先到代理檢視開始一段對話。",
  "work.pickChannel": "選一個頻道以檢視其活動。",
  "work.noEvents": "此頻道尚無事件。",
  "work.state": "狀態",
  "work.goal": "目標",
  "work.noGoal": "尚未設定目標。",
  "work.participants": "參與者",
  "work.state.submitted": "已送出",
  "work.state.working": "進行中",
  "work.state.inputRequired": "待輸入",
  "work.state.completed": "已完成",
  "work.state.failed": "失敗",
  "work.state.canceled": "已取消",
  "work.state.rejected": "已拒絕",
  "work.state.stale": "已過期",
  "work.state.unknown": "未知",
```

- [ ] **Step 2: Wire the surface toggle into DashboardApp**

In `DashboardApp.tsx`, add the import near the other component imports (after line 13's `ConversationsView` import):

```tsx
import { WorkView } from "./work/WorkView";
```

Add state alongside the other `useState` hooks in `DashboardApp()` (near line 341, beside `viewMode`):

```tsx
  const [surface, setSurface] = useState<"agents" | "work">("agents");
```

In the `dashboard__bar-right` block, immediately before the existing `<div className="view-toggle">` (line 579), add the surface toggle:

```tsx
            <div className="surface-toggle">
              <button
                className={surface === "agents" ? "is-active" : ""}
                onClick={() => setSurface("agents")}
              >
                {t("work.toggle.agents")}
              </button>
              <button
                className={surface === "work" ? "is-active" : ""}
                onClick={() => setSurface("work")}
              >
                {t("work.toggle.work")}
              </button>
            </div>
```

Replace the hero + content block so the Work surface swaps in. Wrap lines 612-675 (`<div className="dashboard__hero">` … the closing `</div>` of `dashboard-content`) in a conditional:

```tsx
        {surface === "work" ? (
          <WorkView />
        ) : (
          <>
            {/* existing dashboard__hero + dashboard-content blocks, unchanged */}
          </>
        )}
```

Concretely: insert `{surface === "work" ? (<WorkView />) : (<>` before line 612, and `</>)}` after the `dashboard-content` closing `</div>` (line 675). Leave the bar (538-610), `ConversationsView` (679), and `DetailPanel` (682) outside the conditional so chat + detail still work in both surfaces.

- [ ] **Step 3: Write `work.css`**

`mur-hub-gui/ui/src/styles/components/work.css`:

```css
/* Work view — three-pane observability over the channel store. Sized to fit
   the 1024×768 target: fixed rail + trace, fluid feed. */
.surface-toggle {
  display: inline-flex;
  border: 1px solid var(--border, #2a2a30);
  border-radius: 8px;
  overflow: hidden;
}
.surface-toggle button {
  padding: 4px 12px;
  background: transparent;
  border: 0;
  color: var(--text-muted, #9aa0aa);
  cursor: pointer;
}
.surface-toggle button.is-active {
  background: var(--accent, #4c6ef5);
  color: #fff;
}

.work-view {
  display: grid;
  grid-template-columns: 260px 1fr 280px;
  gap: 1px;
  height: 100%;
  min-height: 0;
  background: var(--border, #2a2a30);
}
.work-list,
.work-feed,
.work-trace {
  background: var(--bg, #161619);
  overflow-y: auto;
  min-height: 0;
}
.work-list__item {
  padding: 10px 12px;
  border-bottom: 1px solid var(--border, #2a2a30);
  cursor: pointer;
}
.work-list__item.is-active {
  background: var(--bg-elev, #1f1f24);
}
.work-list__top {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}
.work-list__title {
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.work-list__sub {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  color: var(--text-muted, #9aa0aa);
  margin-top: 2px;
}
.work-list--empty,
.work-feed--empty,
.work-feed.work-feed--empty {
  padding: 24px;
  color: var(--text-muted, #9aa0aa);
}

.work-badge {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 999px;
  text-transform: lowercase;
  white-space: nowrap;
}
.work-badge--working { background: #1f3a5f; color: #8ec5ff; }
.work-badge--completed { background: #1f3f2f; color: #8ff0b0; }
.work-badge--failed,
.work-badge--rejected { background: #4a2230; color: #ff9bb0; }
.work-badge--inputRequired,
.work-badge--input-required { background: #4a3f1f; color: #ffe08a; }
.work-badge--submitted,
.work-badge--stale,
.work-badge--canceled,
.work-badge--unknown { background: #2a2a30; color: #c8ccd4; }

.work-feed {
  padding: 16px;
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.work-event__author {
  font-size: 12px;
  color: var(--text-muted, #9aa0aa);
  margin-bottom: 2px;
}
.work-event__body { white-space: pre-wrap; }
.work-event--user .work-event__body { color: #d7f5dd; }
.work-event--agent .work-event__body { color: #cfe5ff; }
.work-event--note {
  font-style: italic;
  color: var(--text-muted, #9aa0aa);
}
.work-event--state { text-align: center; }
.work-event__chip {
  font-size: 11px;
  padding: 2px 10px;
  border-radius: 999px;
  background: #2a2a30;
  color: #c8ccd4;
}
.work-event--card {
  border: 1px solid var(--border, #2a2a30);
  border-radius: 8px;
  padding: 8px 10px;
}
.work-event__card-head {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
  margin-bottom: 4px;
}
.work-event__kind { font-weight: 600; color: #ffd479; }
.work-event__payload {
  font-size: 11px;
  background: #0e0e11;
  padding: 6px;
  border-radius: 6px;
  overflow-x: auto;
}

.work-trace { padding: 16px; }
.work-trace__section { margin-bottom: 18px; }
.work-trace__label {
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-muted, #9aa0aa);
  margin-bottom: 6px;
}
.work-trace__criteria { margin: 6px 0 0 16px; font-size: 13px; }
.work-trace__participants { list-style: none; padding: 0; margin: 0; }
.work-trace__participants li {
  display: flex;
  justify-content: space-between;
  padding: 3px 0;
  font-size: 13px;
}
.work-trace__prole { color: var(--text-muted, #9aa0aa); }
```

- [ ] **Step 4: Import the CSS**

Find the CSS barrel and add the import:

```bash
grep -rn "detail-panel.css\|chat.css" mur-hub-gui/ui/src --include=*.css --include=*.ts --include=*.tsx
```

Add `@import "./components/work.css";` (CSS barrel) or `import "./styles/components/work.css";` (TS entry) next to the existing `chat.css`/`detail-panel.css` import, matching whatever pattern that grep reveals.

- [ ] **Step 5: Build + run full frontend test suite**

Run:
```bash
cd mur-hub-gui/ui && npm test && npm run build
```
Expected: vitest green; `tsc -b && vite build` succeeds.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/DashboardApp.tsx \
        mur-hub-gui/ui/src/styles/components/work.css \
        mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
# plus the CSS barrel file the grep identified
git commit -m "feat(hub-work): Agents|Work surface toggle, work.css, i18n

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase C — CLI: channel-aware TUI

### Task C1: `ChannelMeta` + `Session::current()`

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/persist.rs`

- [ ] **Step 1: Write the failing test**

Add a `tests` module at the bottom of `persist.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn current_is_none_until_first_append_then_some() {
        let home = tempdir().unwrap();
        let mut s = Session::create(home.path(), "qa").unwrap();
        assert!(s.current().is_none(), "no channel before first append");
        s.append("user", "hello", None).unwrap();
        let meta = s.current().expect("channel exists after append");
        assert!(!meta.id.is_empty());
        assert!(!meta.state.is_empty() && !meta.state.contains('"'));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core current_is_none_until_first_append_then_some`
Expected: FAIL — `no method named current found for struct Session`.

- [ ] **Step 3: Implement `ChannelMeta` + `Session::current`**

In `persist.rs`, add near `SessionInfo` (after line 27):

```rust
/// Lightweight view of the live channel for the status bar.
#[derive(Debug, Clone)]
pub struct ChannelMeta {
    pub id: String,
    /// kebab-case `ChannelState` (e.g. "working").
    pub state: String,
}
```

Add a method inside `impl Session` (after `channel_id`, ~line 65):

```rust
    /// The live channel's id + state for the status bar. `None` until the first
    /// append creates the channel. Best-effort: a read error yields `None`.
    pub fn current(&self) -> Option<ChannelMeta> {
        let id = self.channel_id.clone()?;
        let ch = self.svc.store().load_manifest(&id).ok()?;
        let state = serde_json::to_string(&ch.state)
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        Some(ChannelMeta { id, state })
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core current_is_none_until_first_append_then_some`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/cli/persist.rs
git commit -m "feat(cli): Session::current() exposes live channel id + state

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task C2: `App.channel` cache + `refresh_channel` + status bar segment

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`
- Modify: `mur-core/src/cmd/agent/cli/ui.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `app.rs`:

```rust
    #[test]
    fn refresh_channel_populates_after_a_turn() {
        let mut a = app();
        assert!(a.channel.is_none(), "no channel before any turn");
        a.begin_user_turn("hi"); // persists the user turn → lazily creates channel
        assert!(
            a.channel.is_some(),
            "channel meta cached after the first persisted turn"
        );
        let meta = a.channel.as_ref().unwrap();
        assert!(!meta.id.is_empty());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core refresh_channel_populates_after_a_turn`
Expected: FAIL — `no field channel on type App`.

- [ ] **Step 3: Implement the cache**

In `app.rs`:

1. Import `ChannelMeta`: change `use super::persist::{Session, TurnRecord};` to:

```rust
use super::persist::{ChannelMeta, Session, TurnRecord};
```

2. Add the field to `struct App` (after `pub session: Session,` ~line 133):

```rust
    /// Cached live-channel id + state for the status bar. Refreshed after each
    /// persisted turn and on resume/switch. `None` until the first append.
    pub channel: Option<ChannelMeta>,
```

3. Initialize it in `App::new` (after `session,` ~line 163):

```rust
            channel: None,
```

4. Add a refresh method inside `impl App` (after `persist_turn`, ~line 194):

```rust
    /// Re-read the live channel meta into the status-bar cache (best-effort).
    pub fn refresh_channel(&mut self) {
        self.channel = self.session.current();
    }
```

5. Call it at the end of `persist_turn` (so the cache tracks lazy creation). Replace the body of `persist_turn` (lines 187-194) with:

```rust
    fn persist_turn(&mut self, role: &str, text: &str, task_id: Option<&str>) {
        match self.session.append(role, text, task_id) {
            Ok(()) => self.channel = self.session.current(),
            Err(e) => {
                if !self.persist_warned {
                    self.persist_warned = true;
                    self.push_system(format!("warning: session is not being saved: {e}"));
                }
            }
        }
    }
```

6. In `start_new_session` (line 300), clear the cache — add after `self.session = session;`:

```rust
        self.channel = None;
```

- [ ] **Step 4: Render the channel segment in the status bar**

In `ui.rs`, inside `render_status`, after the auto-approve block and before `spans.push(Span::styled(msg, …))` (line 173), add:

```rust
    if let Some(meta) = &app.channel {
        let short: String = meta.id.chars().take(8).collect();
        spans.push(Span::styled(
            format!("⌗ {short} · {} ", meta.state),
            Style::default().fg(SYSTEM),
        ));
        spans.push(Span::raw("  "));
    }
```

- [ ] **Step 5: Run tests + build**

Run:
```bash
cargo test -p mur-core refresh_channel_populates_after_a_turn
cargo build -p mur-core
```
Expected: test PASS; builds clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/ui.rs
git commit -m "feat(cli): show live channel id + state in the TUI status bar

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task C3: `/channels` command — list + switch

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/app.rs`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs`

- [ ] **Step 1: Write the failing tests (parse + switch)**

Add to the `tests` module in `app.rs`:

```rust
    #[test]
    fn parse_slash_channels() {
        assert_eq!(parse_slash("/channels"), Some(SlashCmd::Channels(None)));
        assert_eq!(parse_slash("/channels 2"), Some(SlashCmd::Channels(Some(2))));
        assert_eq!(parse_slash("/chan"), Some(SlashCmd::Channels(None)));
        assert_eq!(parse_slash("/channels x"), Some(SlashCmd::Channels(None)));
    }

    #[test]
    fn switch_channel_loads_history_and_caches_meta() {
        // Build a channel with two turns via one session…
        let home = tempdir().unwrap();
        let mut a = app_at(&home);
        a.begin_user_turn("first question");
        a.finish_agent_turn("first answer".into(), Some("t1".into()));
        let first_id = a.channel.as_ref().unwrap().id.clone();

        // …start a fresh conversation (new empty session)…
        let s = Session::create(&a.home, &a.agent).unwrap();
        a.start_new_session(s);
        assert!(a.channel.is_none());

        // …then switch back to the first channel by id.
        a.switch_channel(&first_id).unwrap();
        assert_eq!(a.channel.as_ref().unwrap().id, first_id);
        assert!(
            a.messages.iter().any(|m| m.text == "first question"),
            "history rehydrated on switch"
        );
    }
```

Update the test-helper `app()` so a variant can pin the home dir (add beneath `fn app()`):

```rust
    fn app_at(home: &tempfile::TempDir) -> App {
        let session = Session::create(home.path(), "a").unwrap();
        App::new(home.path().to_path_buf(), "a".into(), session)
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core parse_slash_channels switch_channel_loads_history_and_caches_meta`
Expected: FAIL — `no variant Channels`, `no method switch_channel`.

- [ ] **Step 3: Implement parse + variant + switch**

In `app.rs`:

1. Add the variant to `enum SlashCmd` (after `Sessions,` ~line 76):

```rust
    /// `/channels [n]` — list recent channels (None) or switch to the nth (1-based).
    Channels(Option<usize>),
```

2. In `parse_slash`, add a match arm (after the `"sessions" | "ls"` arm ~line 97):

```rust
        "channels" | "chan" | "ch" => SlashCmd::Channels(
            words.next().and_then(|w| w.parse::<usize>().ok()),
        ),
```

3. Add `"/channels"` to `SLASH_COMMANDS` (and bump the array length `9` → `10`):

```rust
pub const SLASH_COMMANDS: [&str; 10] = [
    "/help",
    "/clear",
    "/card",
    "/sessions",
    "/channels",
    "/auto",
    "/mcp",
    "/skill",
    "/exit",
    "/quit",
];
```

4. Add the `switch_channel` method inside `impl App` (after `start_new_session`, ~line 308):

```rust
    /// Switch the live conversation to an existing channel by id: reopen its
    /// session, clear the transcript, rehydrate its turns, and refresh the
    /// status-bar cache. Any in-flight turn must already be cancelled by caller.
    pub fn switch_channel(&mut self, channel_id: &str) -> anyhow::Result<()> {
        let session = Session::open_existing(&self.home, &self.agent, channel_id)?;
        let turns = super::persist::load(&self.home, channel_id, &self.agent)?;
        self.session = session;
        self.messages.clear();
        self.context_task_id = None;
        self.current_task_id = None;
        self.streaming = false;
        self.hitl = None;
        self.load_history(turns);
        self.refresh_channel();
        Ok(())
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core parse_slash_channels switch_channel_loads_history_and_caches_meta`
Expected: PASS.

- [ ] **Step 5: Handle `/channels` in `mod.rs`**

First read the existing `handle_slash` to match its structure:

```bash
grep -n "SlashCmd::Sessions\|fn handle_slash\|RECENT_LIMIT\|list_recent" mur-core/src/cmd/agent/cli/mod.rs
```

Add a `SlashCmd::Channels(arg)` arm next to the `SlashCmd::Sessions` arm. Bare `/channels` reuses the same listing the `Sessions` arm prints (recent channels via `persist::list_recent(&app.home, &app.agent, RECENT_LIMIT)`), but framed as channels and annotating the live one. `/channels <n>` switches:

```rust
        SlashCmd::Channels(arg) => {
            let recent = persist::list_recent(&app.home, &app.agent, RECENT_LIMIT)
                .unwrap_or_default();
            match arg {
                None => {
                    if recent.is_empty() {
                        app.push_system("no channels yet for this agent");
                    } else {
                        let live = app.channel.as_ref().map(|m| m.id.clone());
                        let mut out = String::from("channels (newest first) — /channels <n> to switch:");
                        for (i, s) in recent.iter().enumerate() {
                            let marker = if Some(&s.id) == live.as_ref() { " ←" } else { "" };
                            out.push_str(&format!(
                                "\n  {}. {} · {} turns · {}{}",
                                i + 1,
                                &s.id.chars().take(8).collect::<String>(),
                                s.turns,
                                s.preview,
                                marker
                            ));
                        }
                        app.push_system(out);
                    }
                }
                Some(n) => {
                    // Cancel any in-flight turn before swapping the session.
                    if app.streaming {
                        if let Some(tid) = app.current_task_id.clone() {
                            cancel_task(&app.home, &app.agent, &tid);
                        }
                        app.finish_partial();
                    }
                    match recent.get(n.wrapping_sub(1)) {
                        Some(s) => {
                            let id = s.id.clone();
                            match app.switch_channel(&id) {
                                Ok(()) => app.push_system(format!(
                                    "switched to channel {} ({} turns)",
                                    &id.chars().take(8).collect::<String>(),
                                    app.messages
                                        .iter()
                                        .filter(|m| matches!(m.role, Role::User | Role::Agent))
                                        .count()
                                )),
                                Err(e) => app.push_system(format!("could not switch: {e}")),
                            }
                        }
                        None => app.push_system(format!(
                            "no channel #{n} — run /channels to see the list"
                        )),
                    }
                }
            }
        }
```

If `Role` is not already imported in `mod.rs`, add it to the `use self::app::{…}` line (it currently imports `App, SlashCmd, parse_slash`):

```rust
use self::app::{App, Role, SlashCmd, parse_slash};
```

Confirm `cancel_task` is imported (it is, from `stream` — line 38). If the `Sessions` arm uses a different listing helper, mirror that instead; the goal is parity with how `/sessions` already renders.

- [ ] **Step 6: Update the HELP string**

In `mod.rs`, in the `HELP` const (line 48), insert `/channels (list/switch)` after `/sessions`:

```rust
const HELP: &str = "commands: /help  /clear (new conversation)  /card  /sessions  /channels (list/switch)  /auto [on|off]  /mcp  /skill  /exit · !cmd runs a local shell command (output shared with the agent) · keys: Enter send · Alt+Enter newline · Ctrl+C cancel/clear · Ctrl+D quit · PageUp/PageDown scroll";
```

- [ ] **Step 7: Build + run the CLI test module**

Run:
```bash
cargo build -p mur-core
cargo test -p mur-core cmd::agent::cli
```
Expected: builds; all `cli` module tests PASS (existing + new).

- [ ] **Step 8: Commit**

```bash
git add mur-core/src/cmd/agent/cli/app.rs mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): /channels lists and switches between channels

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

### Task C4: resume + multiplex verification (cache on resume; panes inherit state)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/mod.rs` (`build_app`)

- [ ] **Step 1: Cache the channel on resume**

In `build_app` (mod.rs:135-168), after the `app.load_history(turns);` line in the resume branch (line 144), add:

```rust
            app.refresh_channel();
```

This makes a resumed session show its channel id + state in the status bar immediately (before the first new turn).

- [ ] **Step 2: Build**

Run: `cargo build -p mur-core`
Expected: clean.

- [ ] **Step 3: Manual verification (multiplex inherits the status bar)**

`murmur a1 a2 a3` spawns one `mur agent cli <name>` process per pane (`multiplex.rs`), each running the full TUI. Because the status bar (Task C2) now shows the live channel id + state, every pane shows its agent's in-channel lifecycle state with **no multiplex.rs change**. Verify manually:

```bash
# build + install, then (with ≥2 agents running):
mur agent cli <agent>            # status bar shows "⌗ <id8> · working" after first message
murmur <a> <b>                   # each pane's status bar shows its own channel + state
```

Document in the commit that multiplex state display is inherited, not separately coded.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/cli/mod.rs
git commit -m "feat(cli): show channel in status bar on resume; multiplex inherits it

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase D — integration, quality gates, docs

### Task D1: cross-surface live-sync check (CLI → Hub Work view)

**Files:** none (verification)

- [ ] **Step 1: Automated backend assertion (already covered)**

The event-sourcing + folding path is covered by `work::tests::list_channels_folds_manifests_and_skips_empty` (A2) and `chat::channel_tests` (pre-existing). The `channel-updated` → UI refresh edge is OS-file-watch + Tauri-event glue that is not unit-testable here.

- [ ] **Step 2: Manual cross-surface verification**

```bash
./build.sh --install            # build Hub + mur with embedded dashboard
# 1. Open MuR Hub → switch to the "Work" tab. Note the channel list.
# 2. In a terminal:  mur agent cli mur   → send a message.
# 3. The Hub Work rail gains/updates that channel within ~1s (file-watch),
#    and if it is the selected channel the feed appends the new turns live.
# 4. Reverse: chat in the Hub Agents view → the CLI `/channels` list shows it.
```

Expected: both directions reflect within ~1 second. Record the result in the PR description.

### Task D2: full quality gates + docs + memory

**Files:**
- Modify: `CLAUDE.md` (CLI surface line), `README.md` (if it documents `mur agent cli` slash commands)

- [ ] **Step 1: Format (workspace + excluded Hub crate separately)**

```bash
cargo fmt
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo fmt --check
cargo fmt --check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
Expected: both clean. (Per [[gotcha_ci_fmt_excluded_crates]] the workspace fmt skips excluded crates — the Hub crate MUST be formatted separately or CI's Format job fails.)

- [ ] **Step 2: Clippy (members + excluded Hub crate)**

```bash
cargo clippy -p mur-channel -p mur-core -- -D warnings
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- -D warnings
```
Expected: no warnings.

- [ ] **Step 3: Rust tests (nextest for the flaky-prone mur-core suite)**

```bash
cargo nextest run -p mur-channel -p mur-core
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml work::
```
Expected: green. (Per `mem:project_mur_core_flaky_tests`, use `nextest`; plain `cargo test --workspace` spuriously fails ~7 mur-core tests. Ignore the 4 pre-existing `conversations::summarize::rollup` LanceDB embedding-dim failures noted in `mem:project_unified_channel_pr433` — they are unrelated to this work.)

- [ ] **Step 4: Frontend tests + build**

```bash
cd mur-hub-gui/ui && npm test && npm run build && cd -
```
Expected: vitest green; build succeeds.

- [ ] **Step 5: Docs**

- In `CLAUDE.md`, the `mur agent <subcommand>` bullet lists CLI features — add `/channels` to the `cli` description (it currently mentions `--resume` and multiplex). One-line edit.
- If `README.md` documents the `mur agent cli` slash commands, add `/channels`. (Grep `README.md` for `/sessions`; if absent, skip.)
- The v2 work is UI-only over the existing store, so the `~/.mur` data-model section needs no change.

- [ ] **Step 6: Commit docs**

```bash
git add CLAUDE.md README.md
git commit -m "docs: note /channels CLI command + v2 Work view

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

- [ ] **Step 7: Update memory**

Append to `mem:project_unified_channel_pr433` (or a new `project_unified_channel_v2` memory) that v2 (Hub Work view + CLI `/channels` + status-bar channel state) is implemented on branch `feat/unified-channel-v2`, scoped to render existing events with forward-compatible event cards; v3 (delegation/HITL/dual-mode) still pending.

---

## Self-Review

**1. Spec coverage (against `2026-06-15-unified-channel-design.md` §6 + Phasing v2 row):**
- "Hub Agents | Work toggle" → Task B6. ✓
- "Work view = left rail (Channel list + state badges)" → WorkChannelList (B4) + stateBadge (B2). ✓
- "center (single event stream with per-agent attribution + collapsible specialist summary cards + inline HITL cards)" → WorkFeed + ChannelEventItem (B3). Per-agent attribution = `actorName`. Specialist-summary / HITL cards = the forward-compatible `card` variant (no v1 producer; renders when v3 emits those kinds) — matches the agreed "render existing events + forward-compatible" scope. ✓
- "right (participants + trace/plan progress)" → WorkTrace (B4) with a participants list + an empty `work-trace__plan` slot for v3 progress. ✓
- "Fits 1024×768 by reusing layout slots" → `work-view` grid is 260/1fr/280; the Work surface replaces the hero+content area inside the existing dashboard frame. ✓
- "CLI: `mur agent cli` opens a Channel" → already true (v1); v2 surfaces it (C2 status bar) + navigation (C3 `/channels`). ✓
- "multiplex panes show each agent's in-Channel lifecycle state, not raw streams" → inherited via the per-pane status bar (C4). ✓
- Cross-surface live sync (Testing section) → reuses the existing watcher; verified in D1. ✓

**2. Placeholder scan:** No "TBD"/"handle edge cases"/"similar to". Every code step shows complete code. Two steps intentionally defer to a `grep` to locate an anchor (the CSS barrel import in B6; the `Sessions` arm shape in C3) because the exact host line is environment-specific — each gives the exact string to add and the command to find where. These are not logic placeholders.

**3. Type consistency:**
- Rust: `ChannelSummary`/`WorkParticipant` defined in A1, consumed by A2/A3; `summary_of(&Channel, &[ChannelEvent])` signature stable. `ChannelMeta{id,state}` defined in C1, consumed by C2/C3. `Session::current()`, `App::refresh_channel()`, `App::switch_channel()`, `SlashCmd::Channels(Option<usize>)` named identically across tasks.
- TS: `ChannelSummary`/`Channel`/`ChannelEvent`/`ChannelActor` in B1; `stateBadge`/`eventVariant`/`eventKindLabel`/`actorName`/`relativeTime` in B2 consumed unchanged in B3/B4/B5. Tauri command names `channel_list`/`channel_events`/`channel_get` match between A3 (Rust) and B5 (`invoke<…>("channel_list" | "channel_events" | "channel_get")`); camelCase arg `channelId` matches Tauri's auto snake→camel mapping of the Rust `channel_id` parameter.
- ⚠ One field to verify during B5 Step 2: `AgentEntry.display_name` vs `displayName`. The build step explicitly checks this and says how to adjust.

No gaps found.
