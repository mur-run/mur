# Unified Chat Phase 1 — Model & Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `mur-channel` a durable channel *purpose*, an event-derived read model (title, preview, unread, HITL), and the three product contracts (`list_conversations`, `list_runs`, `search`) that Hub, TUI, and mobile will all consume — without changing any surface's UI yet.

**Architecture:** `Channel` gains an additive `purpose: Option<ChannelPurpose>`; every creation path writes it explicitly and one pure `effective_purpose()` resolves legacy `None`. The existing disposable SQLite index (`~/.mur/index/channels/channels.db`) grows columns for purpose, agents, preview, counts, and read watermark, kept fresh incrementally by the single existing append funnel (`ChannelService::refresh_read_model`) rather than by rescanning events. Reads never mutate manifests: legacy correction is an explicit `mur channel backfill-purpose --dry-run/--apply` command.

**Tech Stack:** Rust 2024, `rusqlite` 0.32 (bundled SQLite, FTS5 verified available), `serde`/`serde_json`, `chrono`, `anyhow`, `tempfile` for tests.

**Spec:** `docs/superpowers/specs/2026-08-16-unified-chat-redesign-design.md`

## Global Constraints

- **Crates touched in this phase:** `mur-common`, `mur-channel`, and `mur-core` (CLI command only, Task 9). No Hub/mobile/TUI reader changes — those are Phase 2.
- **Additive schema only.** `purpose` is `Option<ChannelPurpose>` with `#[serde(default)]`. `CHANNEL_SCHEMA_VERSION` is **not** bumped in this phase. Old manifests must deserialize.
- **`None` means legacy, never "Conversation".** Never write `Some(Conversation)` as a default for an existing channel outside the explicit backfill command.
- **Reads never write.** No summary, list, or search path may call `store.save_manifest`. Only `append*`, `transition`, creation paths, and `backfill-purpose --apply` write manifests.
- **The index is disposable.** Every derived column must be reproducible by `rebuild_from`. Never store anything in the index that cannot be recomputed from manifests + events.
- **Test runner:** this repo uses `cargo nextest`, not `cargo test` (plain `cargo test` produces ~7 false failures in `mur-core`). For `mur-channel`/`mur-common` either works; the commands below use `cargo nextest run` for consistency. If `nextest` is missing, install with `cargo install cargo-nextest`.
- **`mur-core` build env (Task 9 only):** `mur-core` needs `ORT_STRATEGY=download` and `MUR_WEB_DIST` set, and `nextest -p mur-core` needs `RUST_MIN_STACK=33554432`. Exact commands are given in that task.
- **Lint gate:** `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --check` must pass before each commit. CI runs clippy with `--all-targets`; a local build without it can hide errors.
- **Branch:** work on `feat/unified-chat-phase1`, branched from `main`. `main` is protected.
- **Never `git commit -am`.** This checkout is shared; stage explicit paths only.

---

### Task 1: `ChannelPurpose` on the manifest

**Files:**
- Modify: `mur-common/src/channel.rs` (add enum near `ChannelState` ~line 40-56; add field to `Channel` struct at lines 105-119)
- Test: `mur-common/src/channel.rs` (existing `#[cfg(test)] mod tests` at the bottom)

**Interfaces:**
- Consumes: nothing (first task).
- Produces: `mur_common::channel::ChannelPurpose` with variants `Conversation`, `FleetRun`, `WorkflowRun`; `Channel.purpose: Option<ChannelPurpose>`.

- [ ] **Step 1: Write the failing tests**

Append these to the existing `mod tests` block at the bottom of `mur-common/src/channel.rs`:

```rust
    #[test]
    fn legacy_manifest_without_purpose_deserializes_as_none() {
        // A manifest written before this field existed. Must still load.
        let json = r#"{
            "v": 2,
            "id": "019ed0af-5e38-7912-b554-dc335a8fc2db",
            "title": "chat with mur",
            "state": "working",
            "owner": {"kind": "human", "name": "david"},
            "participants": [],
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).expect("legacy manifest must deserialize");
        assert_eq!(ch.purpose, None, "absent purpose must be None, not a default");
    }

    #[test]
    fn purpose_round_trips_in_kebab_case() {
        let json = r#"{
            "v": 2,
            "id": "x",
            "title": "t",
            "state": "working",
            "owner": {"kind": "system"},
            "participants": [],
            "purpose": "fleet-run",
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        assert_eq!(ch.purpose, Some(ChannelPurpose::FleetRun));

        let back = serde_json::to_string(&ch).unwrap();
        assert!(back.contains(r#""purpose":"fleet-run""#), "got: {back}");
    }

    #[test]
    fn purpose_is_omitted_when_none() {
        let json = r#"{
            "v": 2, "id": "x", "title": "t", "state": "working",
            "owner": {"kind": "system"}, "participants": [],
            "created_at": "2026-08-01T10:00:00Z",
            "updated_at": "2026-08-01T10:00:00Z"
        }"#;
        let ch: Channel = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&ch).unwrap();
        assert!(
            !back.contains("purpose"),
            "a None purpose must not be written back as null: {back}"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-common channel::tests`
Expected: FAIL — `cannot find type ChannelPurpose in this scope` / `no field purpose on type Channel`.

- [ ] **Step 3: Write minimal implementation**

In `mur-common/src/channel.rs`, add the enum immediately after the `ChannelState` enum (which ends around line 56):

```rust
/// Why a channel exists. Deliberately smaller than the ways a UI may render a
/// channel: Direct vs Group is derived from participants, and Companion/HITL
/// are event-derived states — none of them are purposes.
///
/// `Option<ChannelPurpose>` on `Channel`: `None` means "written before this
/// field existed" and MUST NOT be treated as an explicit `Conversation`.
/// Resolve it for display with `mur_channel::purpose::effective_purpose`;
/// correct it on disk only with `mur channel backfill-purpose`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelPurpose {
    Conversation,
    FleetRun,
    WorkflowRun,
}
```

Then add the field to the `Channel` struct, after `pub state: ChannelState,`:

```rust
    /// Why this channel exists. `None` = legacy manifest; see `ChannelPurpose`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<ChannelPurpose>,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-common channel::tests`
Expected: PASS (3 new tests plus the pre-existing ones).

- [ ] **Step 5: Fix every `Channel { .. }` struct literal in the workspace**

Adding a field breaks every literal construction. Find them:

Run: `cargo check --workspace --all-targets 2>&1 | grep -A2 "missing field .purpose."`

Add `purpose: None,` to each **test-fixture** literal it reports (production creation paths get real values in Task 2; using `None` here keeps this task's diff mechanical). Known sites at time of writing: `mur-channel/src/index.rs` test helper `fn ch(...)`, `mur-channel/src/store.rs` tests, `mur-channel/src/service.rs` tests.

⚠️ Workspace-excluded crates (`mur-hub-gui`, `mur-agent-gui`) do not compile under `cargo check --workspace`. Check them separately:

Run: `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml 2>&1 | grep -c "missing field"`
Expected: `0` (the Hub constructs `Channel` only by deserializing, never by literal). If non-zero, fix those literals too.

- [ ] **Step 6: Verify the whole workspace compiles and lints**

Run: `cargo check --workspace --all-targets && cargo clippy -p mur-common -p mur-channel --all-targets -- -D warnings && cargo fmt --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add mur-common/src/channel.rs mur-channel/src/index.rs mur-channel/src/store.rs mur-channel/src/service.rs
git commit -m "feat(channel): add additive ChannelPurpose to the manifest

None means legacy and is never treated as an explicit Conversation."
```

---

### Task 2: Every creation path writes an explicit purpose

**Files:**
- Modify: `mur-channel/src/service.rs:77-106` (`create_for_agent`), `:110-154` (`create_for_fleet`), `:179-197` (`create_for_workflow`)
- Test: `mur-channel/src/service.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `ChannelPurpose` from Task 1.
- Produces: every newly created channel carries `Some(purpose)`. `create_for_agent` now writes an **empty** `title` (Task 5 fills it from the first human message); `create_for_fleet`/`create_for_workflow` keep their existing meaningful titles.

- [ ] **Step 1: Write the failing tests**

Add to `mur-channel/src/service.rs`'s test module:

```rust
    #[test]
    fn creation_paths_write_explicit_purpose() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();

        let chat = svc.create_for_agent("mur").unwrap();
        assert_eq!(chat.purpose, Some(ChannelPurpose::Conversation));

        let wf = svc.create_for_workflow("release").unwrap();
        assert_eq!(wf.purpose, Some(ChannelPurpose::WorkflowRun));
        assert_eq!(wf.title, "workflow: release", "workflow title convention is load-bearing for legacy inference");

        let fleet = svc.create_for_fleet("projectx", "lead", &["a".into()]).unwrap();
        assert_eq!(fleet.purpose, Some(ChannelPurpose::FleetRun));
        assert_eq!(fleet.id, "fleet-projectx");
    }

    #[test]
    fn new_conversation_starts_with_an_empty_title() {
        // The title comes from the first human message (Task 5), not from a
        // useless "chat with {agent}" placeholder that made every row identical.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        assert_eq!(ch.title, "");
    }

    #[test]
    fn purpose_survives_a_manifest_round_trip() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let reloaded = svc.store().load_manifest(&ch.id).unwrap();
        assert_eq!(reloaded.purpose, Some(ChannelPurpose::Conversation));
    }
```

Add `ChannelPurpose` to the test module's `use mur_common::channel::{...}` import list.

⚠️ Check `create_for_fleet`'s real signature before writing the call above:

Run: `sed -n '110,120p' mur-channel/src/service.rs`

Adjust the `create_for_fleet("projectx", "lead", &["a".into()])` call in the test to match the actual parameters.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel creation_paths_write_explicit_purpose new_conversation_starts_with_an_empty_title purpose_survives_a_manifest_round_trip`
Expected: FAIL — assertions compare `None` against `Some(...)`, and the title assertion sees `"chat with mur"`.

- [ ] **Step 3: Write minimal implementation**

In `create_for_agent` (line ~79), change two fields of the `Channel` literal:

```rust
            title: String::new(),
```

and after `state: ChannelState::Working,` add:

```rust
            purpose: Some(ChannelPurpose::Conversation),
```

In `create_for_fleet`'s literal, after `state: ChannelState::Working,`:

```rust
            purpose: Some(ChannelPurpose::FleetRun),
```

In `create_for_workflow`'s literal, after `state: ChannelState::Working,`:

```rust
            purpose: Some(ChannelPurpose::WorkflowRun),
```

Add `ChannelPurpose` to the file's existing `use mur_common::channel::{...}` import.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS. If a pre-existing test asserts `title == "chat with X"`, update that assertion to `""` — the placeholder title is the defect being removed.

- [ ] **Step 5: Verify no other crate asserted the old title**

Run: `grep -rn "chat with " --include=*.rs --include=*.ts --include=*.tsx . | grep -v target | grep -v "^./docs"`
Expected: no production code depends on the string. Report anything found rather than silently changing it.

- [ ] **Step 6: Commit**

```bash
git add mur-channel/src/service.rs
git commit -m "feat(channel): every creation path writes an explicit purpose

Conversations start with an empty title; the first human message names them."
```

---

### Task 3: Centralized legacy classification (`effective_purpose`)

**Files:**
- Create: `mur-channel/src/purpose.rs`
- Modify: `mur-channel/src/lib.rs` (add `pub mod purpose;`)
- Test: `mur-channel/src/purpose.rs` (inline `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `Channel`, `ChannelPurpose` from Task 1.
- Produces: `mur_channel::purpose::effective_purpose(ch: &Channel) -> ChannelPurpose` — pure, never writes. This is the ONLY place inference rules exist; no frontend and no other module may re-implement them.

- [ ] **Step 1: Write the failing test**

Create `mur-channel/src/purpose.rs` with the test module first:

```rust
//! The single owner of legacy channel classification.
//!
//! Channels written before `Channel.purpose` existed carry `None`. Exactly one
//! function resolves that for display, and it NEVER writes: a read path that
//! silently migrates data produces changes nobody can audit. On-disk correction
//! is the explicit `mur channel backfill-purpose` command.

use mur_common::channel::{Channel, ChannelPurpose};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use mur_common::channel::{ChannelActor, ChannelState};

    fn legacy(id: &str, title: &str) -> Channel {
        let now = Utc::now();
        Channel {
            v: 2,
            id: id.to_string(),
            title: title.to_string(),
            goal: Default::default(),
            state: ChannelState::Working,
            owner: ChannelActor::System,
            participants: vec![],
            purpose: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn explicit_purpose_always_wins_over_inference() {
        // An id that *looks* like a fleet but is explicitly a conversation.
        let mut ch = legacy("fleet-projectx", "fleet: projectx");
        ch.purpose = Some(ChannelPurpose::Conversation);
        assert_eq!(effective_purpose(&ch), ChannelPurpose::Conversation);
    }

    #[test]
    fn fleet_id_prefix_implies_fleet_run() {
        let ch = legacy("fleet-projectx", "fleet: projectx");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::FleetRun);
    }

    #[test]
    fn workflow_title_convention_implies_workflow_run() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "workflow: release");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::WorkflowRun);
    }

    #[test]
    fn everything_else_infers_conversation() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "chat with mur");
        assert_eq!(effective_purpose(&ch), ChannelPurpose::Conversation);
    }

    #[test]
    fn inference_is_deterministic_and_pure() {
        let ch = legacy("019ed0af-5e38-7912-b554-dc335a8fc2db", "workflow: release");
        let before = serde_json::to_string(&ch).unwrap();
        let a = effective_purpose(&ch);
        let b = effective_purpose(&ch);
        assert_eq!(a, b);
        assert_eq!(
            before,
            serde_json::to_string(&ch).unwrap(),
            "effective_purpose must not mutate the manifest"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Add `pub mod purpose;` to `mur-channel/src/lib.rs` (after `pub mod index;`), then:

Run: `cargo nextest run -p mur-channel purpose::`
Expected: FAIL — `cannot find function effective_purpose in this scope`.

- [ ] **Step 3: Write minimal implementation**

Insert above the test module in `mur-channel/src/purpose.rs`:

```rust
/// Legacy titles created by `create_for_workflow` before purposes existed.
const WORKFLOW_TITLE_PREFIX: &str = "workflow: ";
/// Stable id prefix minted by `create_for_fleet`.
const FLEET_ID_PREFIX: &str = "fleet-";

/// Resolve a channel's purpose for display.
///
/// Order matters: an explicit stored purpose always wins, so a conversation
/// whose first message happens to start with "workflow:" can never be
/// reclassified once it has been written or backfilled.
pub fn effective_purpose(ch: &Channel) -> ChannelPurpose {
    if let Some(p) = ch.purpose {
        return p;
    }
    if ch.id.starts_with(FLEET_ID_PREFIX) {
        return ChannelPurpose::FleetRun;
    }
    if ch.title.starts_with(WORKFLOW_TITLE_PREFIX) {
        return ChannelPurpose::WorkflowRun;
    }
    ChannelPurpose::Conversation
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p mur-channel purpose::`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/purpose.rs mur-channel/src/lib.rs
git commit -m "feat(channel): centralize legacy purpose inference in one pure fn

Reads resolve purpose; reads never persist it."
```

---

### Task 4: Read-model columns for purpose, agents, and activity

**Files:**
- Modify: `mur-channel/src/index.rs` (`migrate` at lines 65-78, `upsert` at 80-98, `ChannelRow` struct, `list` at 100-116)
- Test: `mur-channel/src/index.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `effective_purpose` (Task 3).
- Produces: `ChannelRow` gains `purpose: String` (kebab), `agents: String` (JSON array), `preview: String`, `msg_count: i64`, `last_seq: i64`, `last_read_seq: i64`, `hitl_pending: bool`. `ChannelIndex::upsert` writes the manifest-derived columns (purpose, agents) and leaves activity columns untouched on update (Task 5 owns those).

- [ ] **Step 1: Write the failing tests**

Add to `mur-channel/src/index.rs`'s test module:

```rust
    #[test]
    fn migrate_adds_columns_to_a_preexisting_v1_database() {
        // Simulate a DB created before these columns existed, then open the
        // index over it. ALTER TABLE must run without destroying rows.
        let tmp = TempDir::new().unwrap();
        // The index lives at <mur_home>/index/channels/channels.db — NOT
        // alongside channel data in <mur_home>/channels/.
        let dir = tmp.path().join("index").join("channels");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("channels.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE channels (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, state TEXT NOT NULL,
                    owner TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO channels VALUES ('old','chat with mur','working','{}','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        let idx = ChannelIndex::open(tmp.path()).expect("must migrate, not fail");
        let rows = idx.list(10).unwrap();
        assert_eq!(rows.len(), 1, "existing row must survive migration");
        assert_eq!(rows[0].id, "old");
        assert_eq!(rows[0].purpose, "conversation", "column DEFAULT until something re-upserts it");
    }

    #[test]
    fn upsert_writes_purpose_and_agents() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let mut c = ch("c1", ChannelState::Working);
        c.purpose = Some(mur_common::channel::ChannelPurpose::Conversation);
        c.participants = vec![Participant {
            actor: ChannelActor::Agent { id: "mur".into() },
            role: ParticipantRole::Delegate,
            joined_at: Utc::now(),
        }];
        idx.upsert(&c).unwrap();

        let rows = idx.list(10).unwrap();
        assert_eq!(rows[0].purpose, "conversation");
        assert_eq!(rows[0].agents, r#"["mur"]"#);
    }

    #[test]
    fn upsert_infers_purpose_for_a_legacy_manifest() {
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let mut c = ch("fleet-projectx", ChannelState::Working);
        c.purpose = None; // legacy
        idx.upsert(&c).unwrap();
        assert_eq!(idx.list(10).unwrap()[0].purpose, "fleet-run");
    }

    #[test]
    fn upsert_does_not_clobber_activity_columns() {
        // Re-upserting a manifest (e.g. a state transition) must not reset the
        // preview/counters that the append path maintains.
        let tmp = TempDir::new().unwrap();
        let idx = ChannelIndex::open(tmp.path()).unwrap();
        let c = ch("c1", ChannelState::Working);
        idx.upsert(&c).unwrap();
        idx.conn_for_test()
            .execute(
                "UPDATE channels SET preview='hello', msg_count=3, last_seq=7 WHERE id='c1'",
                [],
            )
            .unwrap();

        idx.upsert(&c).unwrap();

        let r = &idx.list(10).unwrap()[0];
        assert_eq!(r.preview, "hello");
        assert_eq!(r.msg_count, 3);
        assert_eq!(r.last_seq, 7);
    }
```

Add to the test module's imports: `use mur_common::channel::{Participant, ParticipantRole};`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel index::`
Expected: FAIL — `no field purpose on type ChannelRow`, `no method conn_for_test`.

- [ ] **Step 3: Write minimal implementation**

Extend the `ChannelRow` struct (top of `index.rs`):

```rust
#[derive(Debug, Clone)]
pub struct ChannelRow {
    pub id: String,
    pub title: String,
    pub state: String,
    pub updated_at: String,
    /// `effective_purpose` resolved at write time, kebab-case.
    pub purpose: String,
    /// JSON array of agent participant ids, e.g. `["mur"]`.
    pub agents: String,
    /// Text of the most recent human-visible message.
    pub preview: String,
    /// Human-visible message count (drives unread, never turn totals).
    pub msg_count: i64,
    /// Highest event seq seen.
    pub last_seq: i64,
    /// Read watermark (Task 7).
    pub last_read_seq: i64,
    /// A HITL request is awaiting a response.
    pub hitl_pending: bool,
}
```

Replace `migrate` with a version that creates the full table for new DBs and ALTERs old ones. SQLite has no `ADD COLUMN IF NOT EXISTS`, so ignore the duplicate-column error:

```rust
    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channels (
                id            TEXT PRIMARY KEY,
                title         TEXT NOT NULL,
                state         TEXT NOT NULL,
                owner         TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_updated ON channels(updated_at DESC);",
        )?;
        // Additive columns. `ADD COLUMN` on an existing column errors; that is
        // the "already migrated" case, so it is ignored deliberately.
        for ddl in [
            "ALTER TABLE channels ADD COLUMN purpose TEXT NOT NULL DEFAULT 'conversation'",
            "ALTER TABLE channels ADD COLUMN agents TEXT NOT NULL DEFAULT '[]'",
            "ALTER TABLE channels ADD COLUMN preview TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE channels ADD COLUMN msg_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN last_seq INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN last_read_seq INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE channels ADD COLUMN hitl_pending INTEGER NOT NULL DEFAULT 0",
        ] {
            let _ = self.conn.execute(ddl, []);
        }
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_channels_purpose ON channels(purpose, updated_at DESC);",
        )?;
        Ok(())
    }
```

Rewrite `upsert` so the insert seeds all columns but the update touches only manifest-derived ones:

```rust
    pub fn upsert(&self, ch: &Channel) -> Result<()> {
        let owner = serde_json::to_string(&ch.owner)?;
        let purpose = serde_json::to_string(&crate::purpose::effective_purpose(ch))?
            .trim_matches('"')
            .to_string();
        let agents: Vec<&str> = ch
            .participants
            .iter()
            .filter_map(|p| match &p.actor {
                mur_common::channel::ChannelActor::Agent { id } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let agents = serde_json::to_string(&agents)?;
        self.conn.execute(
            "INSERT INTO channels (id,title,state,owner,created_at,updated_at,purpose,agents)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
               title=excluded.title, state=excluded.state,
               owner=excluded.owner, updated_at=excluded.updated_at,
               purpose=excluded.purpose, agents=excluded.agents",
            rusqlite::params![
                ch.id,
                ch.title,
                serde_json::to_string(&ch.state)?.trim_matches('"'),
                owner,
                ch.created_at.to_rfc3339(),
                ch.updated_at.to_rfc3339(),
                purpose,
                agents,
            ],
        )?;
        Ok(())
    }
```

Update `list`'s SELECT to fetch the new columns:

```rust
    pub fn list(&self, limit: usize) -> Result<Vec<ChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,state,updated_at,purpose,agents,preview,msg_count,last_seq,last_read_seq,hitl_pending
             FROM channels ORDER BY updated_at DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |r| {
                Ok(ChannelRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    state: r.get(2)?,
                    updated_at: r.get(3)?,
                    purpose: r.get(4)?,
                    agents: r.get(5)?,
                    preview: r.get(6)?,
                    msg_count: r.get(7)?,
                    last_seq: r.get(8)?,
                    last_read_seq: r.get(9)?,
                    hitl_pending: r.get::<_, i64>(10)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
```

Add the test-only accessor used by the clobber test, right before the `#[cfg(test)]` module inside `impl ChannelIndex`:

```rust
    /// Raw connection, for tests that need to simulate out-of-band writes.
    #[cfg(test)]
    pub(crate) fn conn_for_test(&self) -> &Connection {
        &self.conn
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel index::`
Expected: PASS. The `migrate_adds_columns_to_a_preexisting_v1_database` test's `purpose` assertion passes because the pre-existing row keeps the column DEFAULT `'conversation'` until something re-upserts it.

- [ ] **Step 5: Verify other `ChannelRow` consumers still compile**

`ChannelRow` gained fields but no field was removed, so struct-literal breakage only happens where tests build one.

Run: `cargo check --workspace --all-targets && cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-channel/src/index.rs
git commit -m "feat(channel): read-model columns for purpose, agents, and activity

Additive ALTER TABLE migration; manifest upserts never clobber activity."
```

---

### Task 5: Incremental activity updates and first-message titles

**Files:**
- Modify: `mur-channel/src/index.rs` (add `record_event`), `mur-channel/src/service.rs:399-411` (`refresh_read_model`), `:198-224` (`append`), and the other append/transition callers of `refresh_read_model`
- Test: `mur-channel/src/service.rs`

**Interfaces:**
- Consumes: Task 4's columns.
- Produces: `ChannelIndex::record_event(&self, ch_id: &str, ev: &ChannelEvent) -> Result<()>` — advances `last_seq`, increments `msg_count` and sets `preview` for human-visible messages, and flips `hitl_pending`. `ChannelService` auto-titles a `Conversation` from its first human message.

- [ ] **Step 1: Write the failing tests**

Add to `mur-channel/src/service.rs`'s test module:

```rust
    #[test]
    fn first_human_message_titles_the_conversation() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append_message(
            &ch.id,
            ChannelActor::local_human(),
            EventKind::Message,
            "explain this repo",
            None,
        )
        .unwrap();

        assert_eq!(svc.store().load_manifest(&ch.id).unwrap().title, "explain this repo");
    }

    #[test]
    fn the_title_is_set_once_and_never_rewritten() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        for text in ["first question", "second question"] {
            svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, text, None)
                .unwrap();
        }
        assert_eq!(svc.store().load_manifest(&ch.id).unwrap().title, "first question");
    }

    #[test]
    fn long_titles_are_truncated_at_the_shared_limit() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let long = "x".repeat(200);
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, &long, None)
            .unwrap();

        let title = svc.store().load_manifest(&ch.id).unwrap().title;
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn cjk_titles_truncate_by_character_not_byte() {
        // Byte-slicing multibyte text panics; this is the regression guard.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let long = "說".repeat(200);
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, &long, None)
            .unwrap();
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().title.chars().count(),
            TITLE_MAX_CHARS
        );
    }

    #[test]
    fn a_fleet_channel_is_never_auto_titled() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let fleet = svc.create_for_fleet("projectx", "lead", &["a".into()]).unwrap();
        svc.append_message(&fleet.id, ChannelActor::local_human(), EventKind::Message, "go", None)
            .unwrap();
        assert_eq!(svc.store().load_manifest(&fleet.id).unwrap().title, "fleet: projectx");
    }

    #[test]
    fn appends_advance_preview_count_and_seq() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();
        svc.append_message(
            &ch.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "hello back",
            None,
        )
        .unwrap();

        let row = svc.index().list(10).unwrap().into_iter().find(|r| r.id == ch.id).unwrap();
        assert_eq!(row.preview, "hello back");
        assert_eq!(row.msg_count, 2);
        assert_eq!(row.last_seq, 2);
    }

    #[test]
    fn internal_events_advance_seq_but_do_not_count_as_messages() {
        // Tool chatter must never inflate a chat's unread badge.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "run it", None)
            .unwrap();
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::ToolCall,
            serde_json::json!({"tool": "bash"}),
            None,
        )
        .unwrap();

        let row = svc.index().list(10).unwrap().into_iter().find(|r| r.id == ch.id).unwrap();
        assert_eq!(row.msg_count, 1, "ToolCall must not count as a message");
        assert_eq!(row.last_seq, 2, "but it does advance the sequence");
        assert_eq!(row.preview, "run it", "and must not become the preview");
    }

    #[test]
    fn hitl_request_sets_pending_and_response_clears_it() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append(&ch.id, ChannelActor::System, EventKind::HitlRequest,
                   serde_json::json!({"hitl_id": "h1"}), None).unwrap();
        let row = |svc: &ChannelService| {
            svc.index().list(10).unwrap().into_iter().find(|r| r.id == ch.id).unwrap()
        };
        assert!(row(&svc).hitl_pending);

        svc.append(&ch.id, ChannelActor::local_human(), EventKind::HitlResponse,
                   serde_json::json!({"hitl_id": "h1", "approved": true}), None).unwrap();
        assert!(!row(&svc).hitl_pending);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel`
Expected: FAIL — `cannot find value TITLE_MAX_CHARS`, titles stay empty, `msg_count`/`preview` stay at defaults.

- [ ] **Step 3: Write the index side**

Add to `mur-channel/src/index.rs`, inside `impl ChannelIndex`:

```rust
    /// Fold one freshly-appended event into the read model.
    ///
    /// Incremental on purpose: rescanning every event on every append is O(n²)
    /// over a conversation's life. `rebuild_from` is the slow, authoritative
    /// path when the index is thrown away.
    pub fn record_event(&self, ch_id: &str, ev: &ChannelEvent) -> Result<()> {
        let counts = matches!(ev.kind, EventKind::Message);
        let preview = if counts {
            ev.payload.get("text").and_then(|v| v.as_str()).unwrap_or("")
        } else {
            ""
        };
        let hitl_delta = match ev.kind {
            EventKind::HitlRequest => Some(1_i64),
            EventKind::HitlResponse => Some(0_i64),
            _ => None,
        };
        self.conn.execute(
            "UPDATE channels SET
               last_seq  = MAX(last_seq, ?2),
               msg_count = msg_count + ?3,
               preview   = CASE WHEN ?3 = 1 THEN ?4 ELSE preview END,
               hitl_pending = COALESCE(?5, hitl_pending)
             WHERE id = ?1",
            rusqlite::params![
                ch_id,
                ev.seq as i64,
                if counts { 1_i64 } else { 0_i64 },
                preview,
                hitl_delta,
            ],
        )?;
        Ok(())
    }
```

Add `EventKind` and `ChannelEvent` to `index.rs`'s `use mur_common::channel::{...}` import.

- [ ] **Step 4: Write the service side**

In `mur-channel/src/service.rs`, add near the top (after the existing `use` block):

```rust
/// Shared truncation limit for auto-derived conversation titles. One constant
/// so TUI, Hub, and mobile cannot disagree about where a title ends.
pub const TITLE_MAX_CHARS: usize = 48;
```

Change `refresh_read_model` to take the event and own both derivations:

```rust
    /// Persist the manifest and fold `ev` into the read model.
    ///
    /// `ev` is `None` only for manifest-only changes (participant edits), which
    /// must not touch activity columns.
    fn refresh_read_model(&self, ch: &Channel, ev: Option<&ChannelEvent>) {
        let res = self
            .store
            .save_manifest(ch)
            .and_then(|()| self.index.upsert(ch))
            .and_then(|()| match ev {
                Some(e) => self.index.record_event(&ch.id, e),
                None => Ok(()),
            });
        if let Err(e) = res {
            tracing::warn!(
                channel_id = %ch.id,
                error = %e,
                "read-model refresh failed after append (event persisted; index is rebuildable)"
            );
        }
    }

    /// The title a conversation should take from `ev`, if any.
    ///
    /// Only untitled Conversations, only the first human `Message`, only when
    /// it has text. Fleet/workflow channels keep their minted titles, and an
    /// attachment-only opener leaves the title empty for the summary layer to
    /// render as `{agent} · {date}`.
    fn derived_title(ch: &Channel, ev: &ChannelEvent) -> Option<String> {
        if crate::purpose::effective_purpose(ch) != ChannelPurpose::Conversation
            || !ch.title.is_empty()
            || ev.kind != EventKind::Message
            || !matches!(ev.actor, ChannelActor::Human { .. })
        {
            return None;
        }
        let text = ev.payload.get("text")?.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        Some(text.chars().take(TITLE_MAX_CHARS).collect())
    }
```

Then in `append`, replace the manifest-refresh block:

```rust
        if let Ok(mut ch) = self.store.load_manifest(channel_id) {
            ch.updated_at = ev.ts;
            if let Some(t) = Self::derived_title(&ch, &ev) {
                ch.title = t;
            }
            self.refresh_read_model(&ch, Some(&ev));
        }
```

- [ ] **Step 5: Update every other `refresh_read_model` caller**

Run: `grep -n "refresh_read_model" mur-channel/src/service.rs`

For each call site: append paths (`append_signed`, `append_message` if it does not delegate to `append`, `append_delegation`, `transition`) pass `Some(&ev)` and apply `derived_title` exactly as `append` does; manifest-only paths (`add_participant`, `remove_participant`) pass `None`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS (all 8 new tests plus existing ones).

- [ ] **Step 7: Lint and commit**

```bash
cargo clippy -p mur-channel --all-targets -- -D warnings && cargo fmt --check
git add mur-channel/src/index.rs mur-channel/src/service.rs
git commit -m "feat(channel): incremental activity read model + first-message titles

Tool events advance seq without inflating message counts."
```

---

### Task 6: `list_conversations` and `list_runs` contracts

**Files:**
- Create: `mur-channel/src/summary.rs`
- Modify: `mur-channel/src/lib.rs` (add `pub mod summary;` and re-exports), `mur-channel/src/service.rs` (add the two methods)
- Test: `mur-channel/src/summary.rs` and `mur-channel/src/service.rs`

**Interfaces:**
- Consumes: Task 4's `ChannelRow` columns, Task 5's activity updates.
- Produces:
  - `mur_channel::summary::ConversationSummary { id, agents: Vec<String>, title: String, preview: String, state: String, updated_at: String, turns: usize, unread: usize, hitl_pending: bool }`
  - `mur_channel::summary::ConversationQuery { agent: Option<String>, active_only: bool }`
  - `mur_channel::summary::RunSummary { id, title, kind: String, state: String, agents: Vec<String>, updated_at: String, hitl_pending: bool }`
  - `ChannelService::list_conversations(&self, q: ConversationQuery) -> Result<Vec<ConversationSummary>>`
  - `ChannelService::list_runs(&self) -> Result<Vec<RunSummary>>`

- [ ] **Step 1: Write the failing tests**

Create `mur-channel/src/summary.rs`:

```rust
//! Product-level read contracts. Every surface renders these; no surface
//! recomputes grouping, classification, or ordering for itself.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use crate::ChannelService;
    use crate::summary::ConversationQuery;
    use mur_common::channel::{ChannelActor, EventKind};
    use tempfile::TempDir;

    fn say(svc: &ChannelService, id: &str, text: &str) {
        svc.append_message(id, ChannelActor::local_human(), EventKind::Message, text, None)
            .unwrap();
    }

    #[test]
    fn active_only_returns_one_row_per_agent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();

        let older = svc.create_for_agent("mur").unwrap();
        say(&svc, &older.id, "old question");
        let newer = svc.create_for_agent("mur").unwrap();
        say(&svc, &newer.id, "new question");
        let other = svc.create_for_agent("qa").unwrap();
        say(&svc, &other.id, "qa question");

        let rows = svc
            .list_conversations(ConversationQuery { agent: None, active_only: true })
            .unwrap();

        assert_eq!(rows.len(), 2, "one row per agent, not per conversation");
        let mur = rows.iter().find(|r| r.agents == vec!["mur".to_string()]).unwrap();
        assert_eq!(mur.id, newer.id, "the newest conversation is the active one");
        assert_eq!(mur.title, "new question");
    }

    #[test]
    fn per_agent_history_returns_every_conversation_newest_first() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let a = svc.create_for_agent("mur").unwrap();
        say(&svc, &a.id, "first");
        let b = svc.create_for_agent("mur").unwrap();
        say(&svc, &b.id, "second");
        let unrelated = svc.create_for_agent("qa").unwrap();
        say(&svc, &unrelated.id, "qa");

        let rows = svc
            .list_conversations(ConversationQuery { agent: Some("mur".into()), active_only: false })
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, b.id, "newest first");
        assert_eq!(rows[1].id, a.id);
    }

    #[test]
    fn fleet_and_workflow_channels_never_appear_in_conversations() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let fleet = svc.create_for_fleet("projectx", "mur", &["mur".into()]).unwrap();
        say(&svc, &fleet.id, "run it");
        let wf = svc.create_for_workflow("release").unwrap();
        say(&svc, &wf.id, "step 1");
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "hello");

        let rows = svc
            .list_conversations(ConversationQuery { agent: None, active_only: false })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, chat.id);

        // The same agent's fleet run is reachable — just not from Chats.
        let runs = svc.list_runs().unwrap();
        assert_eq!(runs.len(), 2);
        assert!(runs.iter().any(|r| r.id == fleet.id && r.kind == "fleet-run"));
        assert!(runs.iter().any(|r| r.id == wf.id && r.kind == "workflow-run"));
    }

    #[test]
    fn active_only_ignores_the_agents_fleet_channel() {
        // Regression: `latest_for_agent` scans every channel a participant
        // appears in, so a recent fleet run used to shadow the real chat.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "hello");
        let fleet = svc.create_for_fleet("projectx", "mur", &["mur".into()]).unwrap();
        say(&svc, &fleet.id, "much later");

        let rows = svc
            .list_conversations(ConversationQuery { agent: Some("mur".into()), active_only: true })
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, chat.id);
    }

    #[test]
    fn empty_channels_are_never_listed() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        svc.create_for_agent("mur").unwrap(); // created, never written to

        let rows = svc
            .list_conversations(ConversationQuery { agent: None, active_only: false })
            .unwrap();
        assert!(rows.is_empty(), "an abandoned draft must not appear in history");
    }

    #[test]
    fn a_conversation_with_no_agent_is_not_shown_as_a_chat() {
        // Legacy workflow channels were created with zero participants, so an
        // inferred Conversation can have no agent. Showing it as a Direct chat
        // would be a row you cannot talk to; it stays reachable via advanced
        // channel tools instead.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let orphan = svc.create_for_workflow("legacy").unwrap();
        say(&svc, &orphan.id, "step 1");
        // Force the legacy shape: no purpose, no workflow title convention.
        let mut m = svc.store().load_manifest(&orphan.id).unwrap();
        m.purpose = None;
        m.title = "something".into();
        svc.store().save_manifest(&m).unwrap();
        svc.index().upsert(&m).unwrap();

        let rows = svc
            .list_conversations(ConversationQuery { agent: None, active_only: false })
            .unwrap();
        assert!(rows.is_empty(), "a zero-agent conversation is not a chat");
    }

    #[test]
    fn a_group_conversation_does_not_hide_its_members_direct_chats() {
        // active_only means "the newest conversation per agent". A multi-agent
        // conversation is its own row and must not consume the slot of every
        // agent in it.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let direct = svc.create_for_agent("mur").unwrap();
        say(&svc, &direct.id, "direct question");
        let group = svc.create_for_agent("mur").unwrap();
        svc.add_participant(&group.id, "qa", mur_common::channel::ParticipantRole::Delegate)
            .unwrap();
        say(&svc, &group.id, "group question");

        let rows = svc
            .list_conversations(ConversationQuery { agent: None, active_only: true })
            .unwrap();

        assert_eq!(rows.len(), 2, "the group row plus mur's direct chat");
        assert!(rows.iter().any(|r| r.id == direct.id));
        assert!(rows.iter().any(|r| r.id == group.id));
    }

    #[test]
    fn a_summary_read_does_not_mutate_the_manifest() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hello");
        let path = tmp.path().join("channels").join(&ch.id).join("channel.yaml");
        let before = std::fs::read_to_string(&path).unwrap();

        let _ = svc
            .list_conversations(ConversationQuery { agent: None, active_only: false })
            .unwrap();

        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }
}
```

The manifest path/format above was verified against `mur-channel/src/store.rs`
before this plan was written: YAML at `<home>/channels/<id>/channel.yaml`.

- [ ] **Step 2: Run tests to verify they fail**

Add `pub mod summary;` to `mur-channel/src/lib.rs`, then:

Run: `cargo nextest run -p mur-channel summary::`
Expected: FAIL — `no method named list_conversations`.

- [ ] **Step 3: Write the summary types**

Add above the test module in `mur-channel/src/summary.rs`:

```rust
/// One row of the Chats inbox or the History drawer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    /// Agent participant ids. One = Direct, more than one = Group; the
    /// distinction is derived here, never stored.
    pub agents: Vec<String>,
    /// Derived from the first human message; may be empty for legacy or
    /// attachment-only conversations (render `{agent} · {date}` then).
    pub title: String,
    /// Text of the most recent message.
    pub preview: String,
    pub state: String,
    pub updated_at: String,
    /// Human-visible message count — NOT an unread badge.
    pub turns: usize,
    /// Messages after the read watermark. This is the badge.
    pub unread: usize,
    pub hitl_pending: bool,
}

/// What slice of conversations a caller wants.
///
/// Hub Chats rows and the mobile list use `{ agent: None, active_only: true }`;
/// the History drawer and TUI `/chats` use `{ agent: Some(x), active_only: false }`.
#[derive(Debug, Clone, Default)]
pub struct ConversationQuery {
    /// `None` = every agent.
    pub agent: Option<String>,
    /// Keep only the most recently updated conversation per agent.
    pub active_only: bool,
}

/// One row of the Work surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub title: String,
    /// `fleet-run` or `workflow-run`.
    pub kind: String,
    pub state: String,
    pub agents: Vec<String>,
    pub updated_at: String,
    pub hitl_pending: bool,
}
```

- [ ] **Step 4: Write the service methods**

Add to `impl ChannelService` in `mur-channel/src/service.rs`:

```rust
    /// Conversation rows for Chats. Ordering is newest-activity-first; empty
    /// channels (created-but-never-sent drafts) are omitted.
    pub fn list_conversations(
        &self,
        q: crate::summary::ConversationQuery,
    ) -> Result<Vec<crate::summary::ConversationSummary>> {
        let mut out: Vec<crate::summary::ConversationSummary> = Vec::new();
        let mut seen_agents: Vec<String> = Vec::new();
        for row in self.index.list(SUMMARY_SCAN_LIMIT)? {
            if row.purpose != "conversation" || row.msg_count == 0 {
                continue;
            }
            let agents: Vec<String> = serde_json::from_str(&row.agents).unwrap_or_default();
            // A conversation with no agent participant cannot be chatted with;
            // legacy workflow channels have this shape. Diagnostics and the
            // advanced channel tools still reach it.
            if agents.is_empty() {
                continue;
            }
            if let Some(want) = &q.agent
                && !agents.iter().any(|a| a == want)
            {
                continue;
            }
            if q.active_only {
                // index.list() is newest-first, so the first Direct row an agent
                // appears in IS its active conversation. Group conversations are
                // their own row and never consume an agent's slot.
                if let [only] = agents.as_slice() {
                    if seen_agents.iter().any(|a| a == only) {
                        continue;
                    }
                    seen_agents.push(only.clone());
                }
            }
            let unread = (row.last_seq - row.last_read_seq).max(0) as usize;
            out.push(crate::summary::ConversationSummary {
                id: row.id,
                agents,
                title: row.title,
                preview: row.preview,
                state: row.state,
                updated_at: row.updated_at,
                turns: row.msg_count as usize,
                unread,
                hitl_pending: row.hitl_pending,
            });
        }
        Ok(out)
    }

    /// Fleet and workflow executions for Work. Never returns conversations.
    pub fn list_runs(&self) -> Result<Vec<crate::summary::RunSummary>> {
        let mut out = Vec::new();
        for row in self.index.list(SUMMARY_SCAN_LIMIT)? {
            if row.purpose == "conversation" {
                continue;
            }
            out.push(crate::summary::RunSummary {
                id: row.id,
                title: row.title,
                kind: row.purpose,
                state: row.state,
                agents: serde_json::from_str(&row.agents).unwrap_or_default(),
                updated_at: row.updated_at,
                hitl_pending: row.hitl_pending,
            });
        }
        Ok(out)
    }
```

Add near `TITLE_MAX_CHARS`:

```rust
/// How many index rows a summary query scans. The index is ordered by activity,
/// so this bounds work while keeping every recently-touched channel reachable.
const SUMMARY_SCAN_LIMIT: usize = 2000;
```

Add the re-export to `mur-channel/src/lib.rs`:

```rust
pub use summary::{ConversationQuery, ConversationSummary, RunSummary};
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS.

⚠️ `unread` here counts every event after the watermark, including tool events. Task 7 replaces that with a message-only count; the `empty_channels_are_never_listed` and ordering assertions above do not depend on it.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p mur-channel --all-targets -- -D warnings && cargo fmt --check
git add mur-channel/src/summary.rs mur-channel/src/lib.rs mur-channel/src/service.rs
git commit -m "feat(channel): list_conversations and list_runs contracts

One API with an explicit query struct, so surfaces cannot disagree."
```

---

### Task 7: Read watermark and true unread counts

**Files:**
- Modify: `mur-channel/src/index.rs` (add `unread_seq` tracking column usage + `mark_read`), `mur-channel/src/service.rs` (`mark_read` passthrough, unread derivation in `list_conversations`)
- Test: `mur-channel/src/service.rs`

**Interfaces:**
- Consumes: Task 6's `ConversationSummary.unread`.
- Produces: `ChannelService::mark_read(&self, channel_id: &str, seq: u64) -> Result<()>` — monotonic. `ConversationSummary.unread` counts only *messages the local human has not authored* after the watermark.

- [ ] **Step 1: Write the failing tests**

Add to `mur-channel/src/service.rs`'s test module:

```rust
    #[test]
    fn unread_counts_only_messages_the_human_did_not_write() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();

        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None).unwrap();
        svc.append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "hello", None).unwrap();
        svc.append(&ch.id, ChannelActor::System, EventKind::ToolCall, serde_json::json!({}), None).unwrap();

        let q = crate::summary::ConversationQuery { agent: None, active_only: false };
        let row = &svc.list_conversations(q).unwrap()[0];
        assert_eq!(row.unread, 1, "one agent message; the human's own turn and the tool call do not count");
    }

    #[test]
    fn mark_read_clears_unread() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None).unwrap();
        let last = svc.append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "hello", None).unwrap();

        svc.mark_read(&ch.id, last.seq).unwrap();

        let q = crate::summary::ConversationQuery { agent: None, active_only: false };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }

    #[test]
    fn the_watermark_never_moves_backwards() {
        // A background window reporting a stale position must not resurrect
        // unread state that a focused window already cleared.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        for _ in 0..3 {
            svc.append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "x", None).unwrap();
        }

        svc.mark_read(&ch.id, 3).unwrap();
        svc.mark_read(&ch.id, 1).unwrap(); // stale surface

        let q = crate::summary::ConversationQuery { agent: None, active_only: false };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }

    #[test]
    fn a_new_agent_message_after_reading_is_unread_again() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        let first = svc.append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "a", None).unwrap();
        svc.mark_read(&ch.id, first.seq).unwrap();
        svc.append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "b", None).unwrap();

        let q = crate::summary::ConversationQuery { agent: None, active_only: false };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 1);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel unread mark_read watermark`
Expected: FAIL — `no method named mark_read`; unread counts are wrong (they include the human's own message and the tool call).

- [ ] **Step 3: Write the implementation**

The unread count must exclude the human's own messages, so counting by seq arithmetic is not enough. Add a dedicated counter column. In `index.rs` `migrate`, add one more `ALTER TABLE` to the existing list:

```rust
            "ALTER TABLE channels ADD COLUMN inbound_seqs TEXT NOT NULL DEFAULT '[]'",
```

`inbound_seqs` is a JSON array of the seqs of messages **not** authored by the local human — small (bounded by message count), trivially rebuildable, and it makes unread a filter rather than a subtraction.

In `record_event`, record inbound message seqs. Replace the `UPDATE` in `record_event` with:

```rust
        let inbound = counts && !matches!(ev.actor, ChannelActor::Human { .. });
        self.conn.execute(
            "UPDATE channels SET
               last_seq  = MAX(last_seq, ?2),
               msg_count = msg_count + ?3,
               preview   = CASE WHEN ?3 = 1 THEN ?4 ELSE preview END,
               hitl_pending = COALESCE(?5, hitl_pending),
               inbound_seqs = CASE WHEN ?6 = 1
                   THEN json_insert(inbound_seqs, '$[#]', ?2)
                   ELSE inbound_seqs END
             WHERE id = ?1",
            rusqlite::params![
                ch_id,
                ev.seq as i64,
                if counts { 1_i64 } else { 0_i64 },
                preview,
                hitl_delta,
                if inbound { 1_i64 } else { 0_i64 },
            ],
        )?;
```

Add `ChannelActor` to `index.rs`'s imports if not already present.

Add `mark_read` to `impl ChannelIndex`:

```rust
    /// Raise the read watermark. Monotonic: a stale surface reporting an older
    /// position can never resurrect already-cleared unread state.
    pub fn mark_read(&self, ch_id: &str, seq: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE channels SET last_read_seq = MAX(last_read_seq, ?2) WHERE id = ?1",
            rusqlite::params![ch_id, seq as i64],
        )?;
        Ok(())
    }
```

Add `inbound_seqs: String` to `ChannelRow`, to `list`'s SELECT (as column index 11) and to its `query_map` closure.

In `service.rs`, add the passthrough:

```rust
    /// Mark everything up to `seq` as read in `channel_id`.
    ///
    /// Callers must only do this for a focused view whose tail is actually
    /// rendered — a background window clearing unread is the bug this rule
    /// exists to prevent.
    pub fn mark_read(&self, channel_id: &str, seq: u64) -> Result<()> {
        self.index.mark_read(channel_id, seq)
    }
```

And in `list_conversations`, replace the `unread` line:

```rust
            let inbound: Vec<i64> = serde_json::from_str(&row.inbound_seqs).unwrap_or_default();
            let unread = inbound.iter().filter(|s| **s > row.last_read_seq).count();
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS.

- [ ] **Step 5: Confirm `json_insert` exists in the bundled SQLite**

The `record_event` UPDATE relies on SQLite's JSON1 functions.

Run: `cargo nextest run -p mur-channel unread_counts_only_messages_the_human_did_not_write -- --nocapture`
Expected: PASS. If it fails with "no such function: json_insert", replace the JSON column with a `channel_inbound (channel_id TEXT, seq INTEGER)` table and an `INSERT`, and count with `SELECT COUNT(*) ... WHERE seq > ?`. Report which path you took.

- [ ] **Step 6: Lint and commit**

```bash
cargo clippy -p mur-channel --all-targets -- -D warnings && cargo fmt --check
git add mur-channel/src/index.rs mur-channel/src/service.rs
git commit -m "feat(channel): monotonic read watermark and true unread counts

Unread counts inbound messages only — never turn totals, never tool events."
```

---

### Task 8: Channel content search

**Files:**
- Modify: `mur-channel/src/index.rs` (FTS5 table + `search`), `mur-channel/src/service.rs` (`search` method), `mur-channel/src/summary.rs` (`SearchScope`, `SearchHit`, `SearchResults`)
- Test: `mur-channel/src/summary.rs`

**Interfaces:**
- Consumes: Tasks 4-7.
- Produces:
  - `mur_channel::summary::SearchScope { Conversations, Runs, All }`
  - `mur_channel::summary::SearchHit { channel_id, seq, title, snippet, purpose, updated_at }`
  - `mur_channel::summary::SearchResults { conversations: Vec<SearchHit>, runs: Vec<SearchHit> }`
  - `ChannelService::search(&self, query: &str, scope: SearchScope) -> Result<SearchResults>`

- [ ] **Step 1: Write the failing tests**

Add a second test module to `mur-channel/src/summary.rs`:

```rust
#[cfg(test)]
mod search_tests {
    use crate::ChannelService;
    use crate::summary::SearchScope;
    use mur_common::channel::{ChannelActor, EventKind};
    use tempfile::TempDir;

    fn say(svc: &ChannelService, id: &str, text: &str) {
        svc.append_message(id, ChannelActor::local_human(), EventKind::Message, text, None)
            .unwrap();
    }

    #[test]
    fn search_finds_message_text_and_locates_the_exact_event() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "opening question");
        say(&svc, &ch.id, "the deploy pipeline broke again");

        let res = svc.search("pipeline", SearchScope::All).unwrap();
        assert_eq!(res.conversations.len(), 1);
        assert_eq!(res.conversations[0].channel_id, ch.id);
        assert_eq!(res.conversations[0].seq, 2, "must point at the matching event");
        assert!(res.conversations[0].snippet.contains("pipeline"));
    }

    #[test]
    fn results_are_grouped_by_surface_not_interleaved() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "shared keyword here");
        let fleet = svc.create_for_fleet("projectx", "mur", &["mur".into()]).unwrap();
        say(&svc, &fleet.id, "shared keyword there");

        let res = svc.search("keyword", SearchScope::All).unwrap();
        assert_eq!(res.conversations.len(), 1);
        assert_eq!(res.runs.len(), 1);
        assert_eq!(res.conversations[0].channel_id, chat.id);
        assert_eq!(res.runs[0].channel_id, fleet.id);
    }

    #[test]
    fn scope_filters_the_result_set() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "shared keyword here");
        let fleet = svc.create_for_fleet("projectx", "mur", &["mur".into()]).unwrap();
        say(&svc, &fleet.id, "shared keyword there");

        let only_chats = svc.search("keyword", SearchScope::Conversations).unwrap();
        assert_eq!(only_chats.conversations.len(), 1);
        assert!(only_chats.runs.is_empty());
    }

    #[test]
    fn search_matches_titles_as_well_as_bodies() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "refactor the parser"); // becomes the title
        say(&svc, &ch.id, "unrelated follow-up");

        let res = svc.search("refactor", SearchScope::Conversations).unwrap();
        assert_eq!(res.conversations.len(), 1);
    }

    #[test]
    fn a_query_with_fts_syntax_characters_does_not_error() {
        // User input goes straight into a MATCH expression; unescaped quotes
        // and operators would otherwise be a hard error mid-typing.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "quoted \"thing\" and (parens)");

        for q in ["\"", "AND", "a OR", "(", "*", ""] {
            let res = svc.search(q, SearchScope::All);
            assert!(res.is_ok(), "query {q:?} must not error: {:?}", res.err());
        }
    }

    #[test]
    fn search_does_not_mutate_manifests() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hello world");
        let path = tmp.path().join("channels").join(&ch.id).join("channel.yaml");
        let before = std::fs::read_to_string(&path).unwrap();

        let _ = svc.search("hello", SearchScope::All).unwrap();

        assert_eq!(before, std::fs::read_to_string(&path).unwrap());
    }
}
```

The manifest is YAML at `<home>/channels/<id>/channel.yaml` (verified).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel search`
Expected: FAIL — `cannot find type SearchScope`.

- [ ] **Step 3: Write the search types**

Add to `mur-channel/src/summary.rs`:

```rust
/// Which surfaces a search should cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    Conversations,
    Runs,
    All,
}

/// One match, located precisely enough to scroll to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub channel_id: String,
    /// Event sequence of the matching message; `0` when only the title matched.
    pub seq: u64,
    pub title: String,
    pub snippet: String,
    pub purpose: String,
    pub updated_at: String,
}

/// Grouped, never interleaved — a Chats hit and a Work hit mean different
/// things and are never presented as one undifferentiated list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchResults {
    pub conversations: Vec<SearchHit>,
    pub runs: Vec<SearchHit>,
}
```

- [ ] **Step 4: Write the FTS index**

In `mur-channel/src/index.rs` `migrate`, after the ALTER loop:

```rust
        // Rebuildable full-text projection of message bodies. `content=''` keeps
        // FTS5 from storing a second copy of the text it indexes.
        self.conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS channel_fts
             USING fts5(channel_id UNINDEXED, seq UNINDEXED, body, tokenize='unicode61');",
        )?;
```

Add to `record_event`, after the `UPDATE`:

```rust
        if counts && !preview.is_empty() {
            self.conn.execute(
                "INSERT INTO channel_fts (channel_id, seq, body) VALUES (?1, ?2, ?3)",
                rusqlite::params![ch_id, ev.seq as i64, preview],
            )?;
        }
```

Add the query method to `impl ChannelIndex`:

```rust
    /// Full-text matches, newest-activity-first. Returns `(channel_id, seq, snippet)`.
    ///
    /// The query is passed to FTS5 as a single quoted phrase: user input is
    /// typed mid-search and must never be interpreted as FTS operator syntax
    /// (a bare `"` would otherwise be a hard error).
    pub fn search_bodies(&self, query: &str, limit: usize) -> Result<Vec<(String, i64, String)>> {
        let phrase = format!("\"{}\"", query.replace('"', " "));
        if phrase.trim_matches(['"', ' ']).is_empty() {
            return Ok(vec![]);
        }
        let mut stmt = self.conn.prepare(
            "SELECT f.channel_id, f.seq, snippet(channel_fts, 2, '', '', '…', 12)
             FROM channel_fts f
             JOIN channels c ON c.id = f.channel_id
             WHERE channel_fts MATCH ?1
             ORDER BY c.updated_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![phrase, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
```

- [ ] **Step 5: Write the service method**

Add to `impl ChannelService`:

```rust
    /// Search channel titles and message bodies, grouped by surface.
    pub fn search(
        &self,
        query: &str,
        scope: crate::summary::SearchScope,
    ) -> Result<crate::summary::SearchResults> {
        use crate::summary::{SearchHit, SearchResults, SearchScope};

        let q = query.trim();
        let mut out = SearchResults::default();
        if q.is_empty() {
            return Ok(out);
        }
        let needle = q.to_lowercase();

        // Index rows carry title + purpose + activity; body hits are keyed by id.
        let rows = self.index.list(SUMMARY_SCAN_LIMIT)?;
        let body_hits = self.index.search_bodies(q, SEARCH_LIMIT)?;

        for row in rows {
            if row.msg_count == 0 {
                continue;
            }
            let is_conversation = row.purpose == "conversation";
            let wanted = match scope {
                SearchScope::All => true,
                SearchScope::Conversations => is_conversation,
                SearchScope::Runs => !is_conversation,
            };
            if !wanted {
                continue;
            }
            let body = body_hits.iter().find(|(id, _, _)| *id == row.id);
            let title_match = row.title.to_lowercase().contains(&needle);
            let (seq, snippet) = match (body, title_match) {
                (Some((_, seq, snip)), _) => (*seq as u64, snip.clone()),
                (None, true) => (0, row.preview.clone()),
                (None, false) => continue,
            };
            let hit = SearchHit {
                channel_id: row.id,
                seq,
                title: row.title,
                snippet,
                purpose: row.purpose,
                updated_at: row.updated_at,
            };
            if is_conversation {
                out.conversations.push(hit);
            } else {
                out.runs.push(hit);
            }
        }
        Ok(out)
    }
```

Add near `SUMMARY_SCAN_LIMIT`:

```rust
/// Max full-text matches returned per query.
const SEARCH_LIMIT: usize = 200;
```

Add the re-export in `lib.rs`:

```rust
pub use summary::{SearchHit, SearchResults, SearchScope};
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS.

- [ ] **Step 7: Lint and commit**

```bash
cargo clippy -p mur-channel --all-targets -- -D warnings && cargo fmt --check
git add mur-channel/src/index.rs mur-channel/src/service.rs mur-channel/src/summary.rs mur-channel/src/lib.rs
git commit -m "feat(channel): FTS5-backed channel search, grouped by surface

User input is phrase-quoted so mid-typing queries never error."
```

---

### Task 9: `mur channel backfill-purpose` migration command

**Files:**
- Create: `mur-core/src/cmd/channel/backfill.rs` — **check first** whether `mur-core/src/cmd/channel.rs` is still a single file; if so, either add the function there (if it stays under 800 lines) or split it into `cmd/channel/{mod,backfill}.rs` following the sibling pattern
- Modify: `mur-core/src/cli/actions.rs:192-206` (`ChannelAction`), `mur-core/src/dispatch.rs:183-192`
- Test: alongside the implementation

**Interfaces:**
- Consumes: `effective_purpose` (Task 3).
- Produces: `mur channel backfill-purpose [--apply] [--limit N]` — dry-run by default; writes `purpose` (and only `purpose`) into legacy manifests.

- [ ] **Step 1: Check the module layout before writing**

Run: `wc -l mur-core/src/cmd/channel.rs`

If under ~700 lines, add the code to that file and skip the split. If not, create `mur-core/src/cmd/channel/mod.rs` (moving the existing contents verbatim) plus `backfill.rs`, and make the move a separate commit before this task's changes — pure code movement first, behavior second.

- [ ] **Step 2: Write the failing test**

Add to the channel command module's test section:

```rust
#[cfg(test)]
mod backfill_tests {
    use super::*;
    use mur_channel::ChannelService;
    use mur_common::channel::{ChannelActor, ChannelPurpose, EventKind};
    use tempfile::TempDir;

    /// Strip `purpose` from a manifest on disk, simulating a legacy channel.
    /// Manifests are YAML (`channel.yaml`), written by `ChannelStore::save_manifest`.
    fn make_legacy(home: &std::path::Path, id: &str) {
        let path = home.join("channels").join(id).join("channel.yaml");
        let mut v: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v.as_mapping_mut()
            .unwrap()
            .remove(serde_yaml::Value::String("purpose".into()));
        std::fs::write(&path, serde_yaml::to_string(&v).unwrap()).unwrap();
    }

    #[test]
    fn dry_run_reports_but_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();
        make_legacy(tmp.path(), &ch.id);

        let report = backfill_purpose(tmp.path(), false, 100).unwrap();

        assert_eq!(report.would_change, 1);
        assert_eq!(report.changed, 0);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            None,
            "a dry run must not touch disk"
        );
    }

    #[test]
    fn apply_writes_the_inferred_purpose() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();
        make_legacy(tmp.path(), &ch.id);

        let report = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(report.changed, 1);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            Some(ChannelPurpose::Conversation)
        );
    }

    #[test]
    fn apply_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();
        make_legacy(tmp.path(), &ch.id);

        backfill_purpose(tmp.path(), true, 100).unwrap();
        let second = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(second.changed, 0, "a second run must find nothing to do");
    }

    #[test]
    fn an_explicit_purpose_is_never_overwritten() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        // A fleet-shaped id that was explicitly recorded as a conversation.
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
            .unwrap();

        let report = backfill_purpose(tmp.path(), true, 100).unwrap();

        assert_eq!(report.changed, 0);
        assert_eq!(
            svc.store().load_manifest(&ch.id).unwrap().purpose,
            Some(ChannelPurpose::Conversation)
        );
    }

    #[test]
    fn limit_bounds_the_batch() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        for _ in 0..3 {
            let ch = svc.create_for_agent("mur").unwrap();
            svc.append_message(&ch.id, ChannelActor::local_human(), EventKind::Message, "hi", None)
                .unwrap();
            make_legacy(tmp.path(), &ch.id);
        }

        let report = backfill_purpose(tmp.path(), true, 2).unwrap();
        assert_eq!(report.changed, 2);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `ORT_STRATEGY=download RUST_MIN_STACK=33554432 cargo nextest run -p mur-core backfill_tests`
Expected: FAIL — `cannot find function backfill_purpose`.

⚠️ If the build fails on a missing embedded web dashboard, set `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` (build it first with `cd ~/Projects/mur-web && npm run build`).

- [ ] **Step 4: Write the implementation**

```rust
use anyhow::Result;
use mur_channel::ChannelService;
use mur_channel::purpose::effective_purpose;
use std::path::Path;

/// What a backfill run did (or would do).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Legacy manifests that would be corrected (dry run).
    pub would_change: usize,
    /// Manifests actually written (apply).
    pub changed: usize,
    /// Manifests already carrying an explicit purpose.
    pub already_set: usize,
}

/// Classify legacy channels and, with `apply`, persist the inferred purpose.
///
/// This is the ONLY path that writes an inferred purpose. Read paths resolve
/// purpose in memory precisely so a listing can never produce an unauditable
/// migration write.
pub fn backfill_purpose(home: &Path, apply: bool, limit: usize) -> Result<BackfillReport> {
    let svc = ChannelService::open(home)?;
    let mut report = BackfillReport::default();

    for id in svc.store().list_ids()? {
        if report.changed >= limit || report.would_change >= limit {
            break;
        }
        let Ok(mut ch) = svc.store().load_manifest(&id) else {
            continue;
        };
        if ch.purpose.is_some() {
            report.already_set += 1;
            continue;
        }
        let inferred = effective_purpose(&ch);
        if apply {
            ch.purpose = Some(inferred);
            svc.store().save_manifest(&ch)?;
            svc.index().upsert(&ch)?;
            report.changed += 1;
            println!("  {id} → {inferred:?}");
        } else {
            report.would_change += 1;
            println!("  {id} → {inferred:?} (dry run)");
        }
    }

    if apply {
        println!(
            "backfilled {} channel(s); {} already had a purpose",
            report.changed, report.already_set
        );
    } else {
        println!(
            "would backfill {} channel(s); {} already have a purpose — re-run with --apply",
            report.would_change, report.already_set
        );
    }
    Ok(report)
}
```

- [ ] **Step 5: Wire up the CLI**

In `mur-core/src/cli/actions.rs`, add to `ChannelAction` (after the `Approve` variant, before the closing brace at line 206):

```rust
    /// Classify legacy channels missing a `purpose` (dry run unless --apply)
    BackfillPurpose {
        /// Write the inferred purposes to disk
        #[arg(long)]
        apply: bool,
        /// Maximum channels to process in this batch
        #[arg(long, default_value_t = 500)]
        limit: usize,
    },
```

In `mur-core/src/dispatch.rs`, add to the `Commands::Channel` match arm (after the `Approve` arm ending at line 191):

```rust
            ChannelAction::BackfillPurpose { apply, limit } => {
                cmd::channel::backfill_purpose(&mur_common::mur_home(), apply, limit)?;
            }
```

⚠️ Confirm the helper that resolves `~/.mur` in this file before writing that line:

Run: `grep -n "mur_home()" mur-core/src/dispatch.rs | head -3`

Use whatever the surrounding arms use.

- [ ] **Step 6: Run tests to verify they pass**

Run: `ORT_STRATEGY=download RUST_MIN_STACK=33554432 cargo nextest run -p mur-core backfill_tests`
Expected: PASS (5 tests).

- [ ] **Step 7: Verify the command is reachable**

Run: `ORT_STRATEGY=download cargo run -p mur-core -- channel backfill-purpose --help`
Expected: help text listing `--apply` and `--limit`.

- [ ] **Step 8: Lint and commit**

```bash
cargo clippy -p mur-core --all-targets -- -D warnings && cargo fmt --check
git add mur-core/src/cmd/channel.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(channel): mur channel backfill-purpose (dry-run by default)

The only path that persists an inferred purpose."
```

(Adjust the staged paths if Step 1 split the module.)

---

### Task 10: `rebuild_from` reproduces every derived column

**Files:**
- Modify: `mur-channel/src/index.rs:126-137` (`rebuild_from`)
- Test: `mur-channel/src/summary.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: `ChannelIndex::rebuild_from(&self, store: &ChannelStore) -> Result<usize>` rebuilds purpose, agents, preview, counts, inbound seqs, HITL state, and the FTS table. `last_read_seq` is deliberately **not** recoverable from events; rebuild preserves it where possible and resets to 0 otherwise.

- [ ] **Step 1: Write the failing test**

Add to `mur-channel/src/summary.rs`'s first test module:

```rust
    #[test]
    fn rebuilding_the_index_reproduces_every_summary_field() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let chat = svc.create_for_agent("mur").unwrap();
        say(&svc, &chat.id, "the deploy pipeline broke");
        svc.append_message(
            &chat.id,
            ChannelActor::Agent { id: "mur".into() },
            EventKind::Message,
            "looking now",
            None,
        )
        .unwrap();
        let fleet = svc.create_for_fleet("projectx", "mur", &["mur".into()]).unwrap();
        say(&svc, &fleet.id, "run it");

        let q = || ConversationQuery { agent: None, active_only: false };
        let before = svc.list_conversations(q()).unwrap();
        let runs_before = svc.list_runs().unwrap();
        let search_before = svc
            .search("pipeline", crate::summary::SearchScope::All)
            .unwrap();

        let n = svc.index().rebuild_from(svc.store()).unwrap();
        assert_eq!(n, 2, "both channels rebuilt");

        let after = svc.list_conversations(q()).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after[0].id, before[0].id);
        assert_eq!(after[0].title, before[0].title);
        assert_eq!(after[0].preview, before[0].preview);
        assert_eq!(after[0].turns, before[0].turns);
        assert_eq!(after[0].unread, before[0].unread);

        assert_eq!(svc.list_runs().unwrap().len(), runs_before.len());

        let search_after = svc
            .search("pipeline", crate::summary::SearchScope::All)
            .unwrap();
        assert_eq!(search_after.conversations.len(), search_before.conversations.len());
        assert_eq!(search_after.conversations[0].seq, search_before.conversations[0].seq);
    }

    #[test]
    fn rebuilding_preserves_the_read_watermark() {
        // The watermark is the one derived value events cannot regenerate;
        // losing it on rebuild would resurface everything as unread.
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        say(&svc, &ch.id, "hi");
        let last = svc
            .append_message(&ch.id, ChannelActor::Agent { id: "mur".into() }, EventKind::Message, "hello", None)
            .unwrap();
        svc.mark_read(&ch.id, last.seq).unwrap();

        svc.index().rebuild_from(svc.store()).unwrap();

        let q = ConversationQuery { agent: None, active_only: false };
        assert_eq!(svc.list_conversations(q).unwrap()[0].unread, 0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-channel rebuild`
Expected: FAIL — after rebuild, preview/turns/search are empty because `rebuild_from` only upserts manifests.

- [ ] **Step 3: Read the current implementation before changing it**

Run: `sed -n '126,138p' mur-channel/src/index.rs`

- [ ] **Step 4: Write the implementation**

Replace `rebuild_from` in `mur-channel/src/index.rs`:

```rust
    /// Drop every derived row and re-derive from manifests + event logs.
    ///
    /// Read watermarks are the exception: nothing in the event log records what
    /// a human has looked at, so they are carried across rather than lost.
    pub fn rebuild_from(&self, store: &ChannelStore) -> Result<usize> {
        let mut watermarks: Vec<(String, i64)> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, last_read_seq FROM channels WHERE last_read_seq > 0")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            watermarks = rows;
        }

        self.conn
            .execute_batch("DELETE FROM channels; DELETE FROM channel_fts;")?;

        let mut n = 0;
        for id in store.list_ids()? {
            let Ok(ch) = store.load_manifest(&id) else {
                continue;
            };
            self.upsert(&ch)?;
            for ev in store.load_events(&id).unwrap_or_default() {
                self.record_event(&id, &ev)?;
            }
            n += 1;
        }

        for (id, seq) in watermarks {
            self.mark_read(&id, seq as u64)?;
        }
        Ok(n)
    }
```

⚠️ `let mut watermarks` followed by an unconditional assignment will trip clippy. Write it as a single `let watermarks = { ... };` block instead.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p mur-channel`
Expected: PASS (the full crate suite).

- [ ] **Step 6: Verify against real data**

The index is disposable, so a rebuild against the live home is safe and is the only check that exercises 268 real channels.

```bash
cp ~/.mur/index/channels/channels.db /tmp/channels.db.bak
ORT_STRATEGY=download cargo run -p mur-core -- internals reindex
```

Then confirm the rebuild classified real channels:

```bash
sqlite3 ~/.mur/index/channels/channels.db \
  "SELECT purpose, COUNT(*) FROM channels GROUP BY purpose;"
```

Expected: a `conversation` bucket plus `fleet-run`/`workflow-run` buckets — not everything in one bucket. If `internals reindex` does not route to `ChannelIndex::rebuild_from`, say so and rebuild via a one-off test instead of changing reindex in this task.

Restore if anything looks wrong: `cp /tmp/channels.db.bak ~/.mur/index/channels/channels.db`

- [ ] **Step 7: Lint and commit**

```bash
cargo clippy -p mur-channel --all-targets -- -D warnings && cargo fmt --check
git add mur-channel/src/index.rs mur-channel/src/summary.rs
git commit -m "feat(channel): rebuild_from re-derives every read-model column

Read watermarks are carried across; nothing else is stored that events cannot regenerate."
```

---

## Phase 1 exit check

Run once, after Task 10:

- [ ] `cargo nextest run -p mur-common -p mur-channel` — all green
- [ ] `ORT_STRATEGY=download RUST_MIN_STACK=33554432 cargo nextest run -p mur-core channel` — all green
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean
- [ ] `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` — clean (workspace-excluded crates are not covered by `--workspace`)
- [ ] `cargo fmt --check` — clean
- [ ] `grep -rn "save_manifest" mur-channel/src/summary.rs mur-channel/src/purpose.rs` — no matches (reads never write)
- [ ] Open a PR against `main`; do not merge to `main` directly.

**Not in this phase** (Phase 2 and later, per spec §15): switching TUI `persist::list_recent`, Hub `channel_list`/`persist_exchange`, and daemon `channel_query` onto these contracts; binding chat surfaces to explicit conversation ids; any navigation, History drawer, `/chats` command, or ⌘K change; retiring `mobile-events.jsonl`; running the backfill against real data.
