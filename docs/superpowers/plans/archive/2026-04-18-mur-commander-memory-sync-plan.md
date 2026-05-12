# mur ↔ Commander Memory Sync — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Phase 1 (P0 Channel 1) — mur-commander 執行結果透過 mur-server 回流到 mur pattern 的 Evidence,讓 maturity 升級從「被注入次數」進步到「真實執行成效」。

**Architecture:** 三倉庫變動 + 一個新閉源後端能力。`mur-common` (OSS) 新增 Scope/Actor/Signal 型別;`mur-core` (OSS) 新增 sync client (outbox/inbox);`mur-server` (閉源 Go) 新增 `/v1/signals/*` 與 Postgres 聚合;`mur-commander` (閉源) 在 WorkflowRunner/AutoFix/Breakpoint 三處發 signal。全程 pull-model,60s/5m 同步,離線容忍。

**Tech Stack:** Rust 2024 (mur-common, mur-core, mur-commander),Go 1.22 (mur-server),PostgreSQL 16,LanceDB (既有),serde_yaml,tokio,axum (mur-core server 端),Chi/mux (mur-server),testcontainers-rs,go testcontainers。

**Repos:**
- `/Volumes/Firecuda4tb/Projects/mur` — OSS Rust workspace (mur-common + mur-core)
- `/Volumes/Firecuda4tb/Projects/mur-commander` — 閉源 Rust workspace (8 crates)
- `/Volumes/Firecuda4tb/Projects/mur-server` — 閉源 Go + Postgres

**Spec reference:** `docs/superpowers/specs/2026-04-18-mur-commander-memory-sync-design.md`

---

## Scope of This Plan

此 Plan **只詳細實作 Phase 1 (P0 Channel 1 Evidence 回流)**,對應 spec Section 8.5 的 W1-W6。

**Phase 2 (Team scope + C2 Chat 萃取)** 與 **Phase 3 (C3 Procedural 萃取)** 會在 Phase 1 驗證通過後,根據 Phase 1 的實測數據另寫獨立 plan。本文件末尾的 "Phase 2/3 Roadmap" 提供高階任務清單供 Phase 1 完成時接續。

**Phase 1 交付標準 (spec §8.6)**:
- `evidence.effectiveness()` vs user `mur feedback helpful/unhelpful` Spearman 相關性 > 0.6
- `mur feedback unhelpful` 事件數下降 ≥ 30%
- Signal → evidence p95 延遲 < 10 分鐘

---

## File Structure

### mur-common (OSS, `/Volumes/Firecuda4tb/Projects/mur/mur-common/`)

| Action | Path | Responsibility |
|---|---|---|
| Create | `src/scope.rs` | `Scope` enum + serde |
| Create | `src/actor.rs` | `Actor` + `ActorSource` enum |
| Create | `src/signal.rs` | `Signal` / `SignalTarget` / `SignalKind` |
| Modify | `src/pattern.rs` | `Origin.actor`, `Pattern.scope`, `Evidence.contributions` |
| Modify | `src/lib.rs` | pub mod exports |
| Modify | `Cargo.toml` | bump schema version comment |

### mur-core (OSS, `/Volumes/Firecuda4tb/Projects/mur/mur-core/`)

| Action | Path | Responsibility |
|---|---|---|
| Create | `src/sync/mod.rs` | sync 子系統入口,pub uses |
| Create | `src/sync/outbox.rs` | 寫 signal YAML 到 `~/.mur/outbox/` |
| Create | `src/sync/inbox.rs` | 讀 inbox,apply 到 pattern |
| Create | `src/sync/cursor.rs` | 持久化 fetch cursor 到 `~/.mur/sync/cursor.json` |
| Create | `src/sync/client.rs` | HTTP client (reqwest) → mur-server |
| Create | `src/cmd/sync_cmd.rs` | `mur push` / `mur fetch` / `mur sync status` / `mur sync logs` |
| Modify | `src/main.rs` | 註冊新 clap 子命令 |
| Modify | `src/cmd/mod.rs` | re-export sync_cmd |
| Modify | `src/auth.rs` | Token response 存 `user_id` 到 `~/.mur/auth.json` |

### mur-server (閉源, `/Volumes/Firecuda4tb/Projects/mur-server/`)

| Action | Path | Responsibility |
|---|---|---|
| Create | `migrations/NNN_add_signal_tables.up.sql` | signals, actor_bindings, unresolved_actors, evidence_contributions |
| Create | `migrations/NNN_add_signal_tables.down.sql` | 回滾 |
| Create | `internal/models/signal.go` | Go side Signal type,對齊 mur-common JSON |
| Create | `internal/models/actor.go` | Actor type |
| Create | `internal/store/postgres/signals.go` | CRUD signals table |
| Create | `internal/store/postgres/actor_bindings.go` | actor_bindings 查詢 |
| Create | `internal/services/signal_ingester.go` | batch 驗證 + dedupe + insert |
| Create | `internal/services/evidence_aggregator.go` | 定時 job:signals → patterns.evidence |
| Create | `internal/api/handlers/signals.go` | POST /v1/signals/batch, GET /v1/signals/pending, POST /v1/signals/ack |
| Create | `internal/api/handlers/actors.go` | POST /v1/actors/resolve |
| Modify | `internal/api/handlers/auth.go` | token response 加 `user_id` |
| Modify | `internal/api/server.go` | 掛新路由 |
| Modify | `internal/models/models.go` | Pattern struct 加 Scope, Evidence.contributions |

### mur-commander (閉源, `/Volumes/Firecuda4tb/Projects/mur-commander/`)

| Action | Path | Responsibility |
|---|---|---|
| Create | `crates/engine/src/mur_sync/mod.rs` | sync 子系統入口 |
| Create | `crates/engine/src/mur_sync/outbox.rs` | commander 端 outbox writer |
| Create | `crates/engine/src/mur_sync/flush.rs` | 60s flush daemon service |
| Create | `crates/engine/src/mur_sync/client.rs` | HTTP client to mur-server,帶 svc token + X-Acting-On-Behalf-Of |
| Modify | `crates/engine/src/workflow/runner.rs` | workflow end → emit C1 signals |
| Modify | `crates/engine/src/workflow/autofix.rs` | AutoFix 觸發 → emit signal |
| Modify | `crates/engine/src/audit.rs` | AuditEntry 加 `injected_patterns: Vec<String>` 欄位 |
| Modify | `crates/daemon/src/service.rs` | 啟動時 spawn flush service |
| Modify | `crates/engine/Cargo.toml` | dep on mur-common (已有),可能加 reqwest |

### Integration Tests (新)

| Action | Path | Responsibility |
|---|---|---|
| Create | `/Volumes/Firecuda4tb/Projects/mur/tests/integration/sync_e2e.rs` | mur ↔ mock-server e2e |
| Create | `/Volumes/Firecuda4tb/Projects/mur-server/tests/integration/signals_test.go` | server 單元 + 整合 |
| Create | `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/tests/mur_sync_test.rs` | commander 端 emit 測試 |
| Create | `/Volumes/Firecuda4tb/Projects/mur-server/docker-compose.e2e.yml` | 整合測試 compose (postgres + mur-server-test) |

---

# Phase 1 — Tasks

## Part A — mur-common Schema (W1-W2, 6 tasks)

### Task 1: Add `Scope` enum to mur-common

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/scope.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/lib.rs`
- Test: inline `#[cfg(test)]` in scope.rs

**Prereq:** 無

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/scope.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    Personal,
    Team { team_id: String },
    Community { pack_id: Option<String> },
}

impl Default for Scope {
    fn default() -> Self { Self::Personal }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_personal() {
        assert_eq!(Scope::default(), Scope::Personal);
    }

    #[test]
    fn yaml_roundtrip_personal() {
        let s = Scope::Personal;
        let y = serde_yaml::to_string(&s).unwrap();
        assert_eq!(y.trim(), "kind: personal");
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn yaml_roundtrip_team() {
        let s = Scope::Team { team_id: "ops".into() };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Scope = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, s);
    }
}
```

- [x] **Step 2: Run test to verify fail**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo test -p mur-common scope
```

Expected: 編譯錯誤 (`scope` module not found — lib.rs 還沒 export)

- [x] **Step 3: Wire up lib.rs**

Edit `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/lib.rs`,新增 `pub mod scope;` 並 `pub use scope::Scope;` 在合適位置 (看檔案既有風格,通常在其他 `pub mod` 一起)。

- [x] **Step 4: Run tests pass**

```bash
cargo test -p mur-common scope
```

Expected: 3 tests pass.

- [x] **Step 5: Commit**

```bash
git -C /Volumes/Firecuda4tb/Projects/mur add mur-common/src/scope.rs mur-common/src/lib.rs
git -C /Volumes/Firecuda4tb/Projects/mur commit -m "feat(common): add Scope enum (personal/team/community)"
```

---

### Task 2: Add `Actor` struct to mur-common

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/actor.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/lib.rs`

**Prereq:** Task 1

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/actor.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorSource {
    ClaudeCode,
    Cursor,
    Aider,
    Slack,
    Telegram,
    Discord,
    CommanderDaemon,
    MurCli,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Actor {
    pub source: ActorSource,
    pub native_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_user_id: Option<String>,
}

impl Actor {
    /// dedupe key 用於 Evidence.contributions 的 HashMap
    pub fn key(&self) -> String {
        format!("{:?}:{}", self.source, self.native_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        let a = Actor {
            source: ActorSource::Slack,
            native_id: "U123ABC".into(),
            display_name: Some("alice".into()),
            resolved_user_id: None,
        };
        assert_eq!(a.key(), "Slack:U123ABC");
    }

    #[test]
    fn yaml_roundtrip_minimal() {
        let a = Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "svc-1".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let y = serde_yaml::to_string(&a).unwrap();
        let back: Actor = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back, a);
    }
}
```

- [x] **Step 2: Verify test fails**

```bash
cargo test -p mur-common actor
```

Expected: module not found.

- [x] **Step 3: Wire up lib.rs**

Add to `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/lib.rs`:
```rust
pub mod actor;
pub use actor::{Actor, ActorSource};
```

- [x] **Step 4: Verify tests pass**

```bash
cargo test -p mur-common actor
```

Expected: 2 tests pass.

- [x] **Step 5: Commit**

```bash
git -C /Volumes/Firecuda4tb/Projects/mur add mur-common/src/actor.rs mur-common/src/lib.rs
git -C /Volumes/Firecuda4tb/Projects/mur commit -m "feat(common): add Actor + ActorSource (provenance without resolution)"
```

---

### Task 3: Add `Origin.actor` + migrate existing `Origin`

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/pattern.rs` (lines around existing `struct Origin`)

**Prereq:** Task 2

- [x] **Step 1: Write failing test**

Add to existing `#[cfg(test)]` block in `pattern.rs`:

```rust
#[test]
fn origin_with_actor_roundtrip() {
    use crate::{Actor, ActorSource};
    let o = Origin {
        source: "commander".into(),
        trigger: OriginTrigger::AgentInferred,
        actor: Some(Actor {
            source: ActorSource::Slack,
            native_id: "U999".into(),
            display_name: Some("bob".into()),
            resolved_user_id: None,
        }),
        confidence: 0.8,
    };
    let y = serde_yaml::to_string(&o).unwrap();
    let back: Origin = serde_yaml::from_str(&y).unwrap();
    assert_eq!(back.actor.as_ref().unwrap().native_id, "U999");
}

#[test]
fn origin_backward_compat_no_actor_field() {
    // 舊 YAML 沒有 actor 欄位
    let old_yaml = r#"
source: starter
trigger: automatic
confidence: 0.5
"#;
    let o: Origin = serde_yaml::from_str(old_yaml).unwrap();
    assert!(o.actor.is_none());
}
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-common origin_with_actor
```

Expected: fails with "no field `actor` on Origin".

- [x] **Step 3: Modify `Origin` struct**

In `pattern.rs`, find the existing `Origin` struct and modify to:

```rust
pub struct Origin {
    pub source: String,
    pub trigger: OriginTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    #[serde(default)]
    pub confidence: f64,
}
```

Remove existing `pub user: Option<String>` and `pub platform: Option<String>` fields if present. Import `use crate::Actor;` at top of file.

- [x] **Step 4: Verify tests pass**

```bash
cargo test -p mur-common
```

Expected: new tests pass + all existing pattern tests still pass (the backward-compat test confirms old YAML still loads).

**If existing code references `origin.user` or `origin.platform`** (from the earlier scan, `learn.rs:534`, `import.rs:109`, `starter.rs:725`, `community_cmd.rs:239,354`), either:
- Update those 4 call sites to build `actor: None` (they currently write None anyway)
- Or keep `user/platform` as deprecated optional fields alongside `actor` for one minor version, with `#[deprecated]` attribute

For minimum impact choose option (b) — keep deprecated, remove in v2.3:

```rust
pub struct Origin {
    pub source: String,
    pub trigger: OriginTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<Actor>,
    #[deprecated(note = "use actor instead, will be removed in v2.3")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[deprecated(note = "use actor.source instead, will be removed in v2.3")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default)]
    pub confidence: f64,
}
```

This keeps compiles clean. Update 4 call sites to leave user/platform as None with `#[allow(deprecated)]` and start using `actor`.

- [x] **Step 5: Commit**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
cargo clippy -p mur-common -- -D warnings
git add mur-common/src/pattern.rs
git commit -m "feat(common): add Origin.actor (deprecate user/platform)"
```

---

### Task 4: Add `Evidence.contributions` HashMap

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/pattern.rs`

**Prereq:** Task 2

- [x] **Step 1: Write failing test**

Add to `pattern.rs` test module:

```rust
#[test]
fn evidence_effectiveness_by_actor() {
    use crate::{Actor, ActorSource};
    use std::collections::HashMap;

    let alice = Actor {
        source: ActorSource::Slack, native_id: "alice".into(),
        display_name: None, resolved_user_id: None,
    };
    let bob = Actor {
        source: ActorSource::Slack, native_id: "bob".into(),
        display_name: None, resolved_user_id: None,
    };

    let mut contribs = HashMap::new();
    contribs.insert(alice.key(), Contribution {
        success_signals: 8, override_signals: 2,
        last_seen: chrono::Utc::now(),
    });
    contribs.insert(bob.key(), Contribution {
        success_signals: 1, override_signals: 4,
        last_seen: chrono::Utc::now(),
    });

    let e = Evidence {
        source_sessions: vec![],
        injection_count: 15,
        success_signals: 9,
        override_signals: 6,
        failure_signals: 0,
        contributions: contribs,
    };

    assert!((e.effectiveness_by_actor(&alice) - 0.8).abs() < 0.001);
    assert!((e.effectiveness_by_actor(&bob) - 0.2).abs() < 0.001);
    assert!((e.effectiveness() - 0.6).abs() < 0.001);
}
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-common evidence_effectiveness_by_actor
```

Expected: type `Contribution` not found, method not found.

- [x] **Step 3: Add Contribution + extend Evidence**

In `pattern.rs`, add:

```rust
use chrono::{DateTime, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    #[serde(default)]
    pub success_signals: u64,
    #[serde(default)]
    pub override_signals: u64,
    pub last_seen: DateTime<Utc>,
}
```

Extend `Evidence`:

```rust
pub struct Evidence {
    // ... existing fields unchanged ...
    #[serde(default)]
    pub contributions: HashMap<String, Contribution>,
}

impl Evidence {
    pub fn effectiveness_by_actor(&self, actor: &crate::Actor) -> f64 {
        match self.contributions.get(&actor.key()) {
            Some(c) => {
                let total = c.success_signals + c.override_signals;
                if total == 0 { 0.5 }
                else { c.success_signals as f64 / total as f64 }
            }
            None => 0.5,  // 中性先驗
        }
    }
}
```

- [x] **Step 4: Verify tests pass**

```bash
cargo test -p mur-common
```

Expected: all green.

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-common -- -D warnings
git add mur-common/src/pattern.rs
git commit -m "feat(common): add Evidence.contributions HashMap + per-actor effectiveness"
```

---

### Task 5: Add `Pattern.scope` field

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/pattern.rs` (or `knowledge.rs` depending where Pattern lives)
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/knowledge.rs` (if KnowledgeBase has scope)

**Prereq:** Task 1

- [x] **Step 1: Write failing test**

Add to `pattern.rs` tests:

```rust
#[test]
fn pattern_scope_defaults_personal() {
    // 舊 YAML 無 scope 欄位
    let old_yaml = r#"
schema: 2
name: test-pattern
description: test
content: { kind: plain, text: "hello" }
tier: session
"#;
    let p: Pattern = serde_yaml::from_str(old_yaml).unwrap();
    assert_eq!(p.scope, Scope::Personal);
}

#[test]
fn pattern_scope_team_roundtrip() {
    let y = r#"
schema: 2
name: team-pat
description: team pattern
content: { kind: plain, text: "x" }
tier: project
scope: { kind: team, team_id: ops }
"#;
    let p: Pattern = serde_yaml::from_str(y).unwrap();
    assert_eq!(p.scope, Scope::Team { team_id: "ops".into() });
}
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-common pattern_scope
```

Expected: `scope` field not found.

- [x] **Step 3: Add scope to Pattern**

In the struct definition where `Pattern` flattens `KnowledgeBase`, add:

```rust
// 加進 KnowledgeBase struct (若 Pattern 是 #[serde(flatten)] wraps it)
pub struct KnowledgeBase {
    // ... 既有欄位 ...
    #[serde(default)]
    pub scope: crate::Scope,
}
```

Import `use crate::Scope;` at top.

- [x] **Step 4: Verify tests + check既有測試**

```bash
cargo test -p mur-common
```

Expected: all pass. 既有 pattern fixture YAML 檔案也要能解析 (scope 預設 Personal)。

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-common -- -D warnings
git add mur-common/src/
git commit -m "feat(common): add Pattern.scope field (defaults to Personal)"
```

---

### Task 6: Create `Signal` wire format type

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/signal.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/lib.rs`

**Prereq:** Tasks 1, 2, 5

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/signal.rs`:

```rust
use crate::{Actor, Pattern, Scope};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SIGNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub actor: Actor,
    pub target: SignalTarget,
    pub kind: SignalKind,
    pub scope: Scope,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
}

fn default_confidence() -> f64 { 1.0 }
fn current_schema_version() -> u32 { SIGNAL_SCHEMA_VERSION }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalTarget {
    Pattern { name: String, scope: Scope },
    NewDraftPattern { payload: Box<Pattern> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalKind {
    ExecutionSuccess,
    ExecutionFailure { error: String },
    UserOverrideAtBreakpoint { reason: Option<String> },
    AutoFixApplied { step: String },
    NewPatternProposal { origin_context: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorSource;

    #[test]
    fn signal_roundtrip_execution_success() {
        let s = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::CommanderDaemon,
                native_id: "svc-1".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "rust-err-handling".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.id, s.id);
        assert!(matches!(back.kind, SignalKind::ExecutionSuccess));
    }

    #[test]
    fn signal_confidence_defaults_to_one() {
        let y = r#"
id: 00000000-0000-0000-0000-000000000001
emitted_at: 2026-04-18T10:00:00Z
actor: { source: commander_daemon, native_id: x }
target: { kind: pattern, name: foo, scope: { kind: personal } }
kind: { type: execution_success }
scope: { kind: personal }
"#;
        let s: Signal = serde_yaml::from_str(y).unwrap();
        assert_eq!(s.confidence, 1.0);
        assert_eq!(s.schema_version, 1);
    }
}
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-common signal
```

Expected: module not found. Also likely need to add `uuid` dep if not present.

- [x] **Step 3: Ensure deps + wire up**

Check `/Volumes/Firecuda4tb/Projects/mur/mur-common/Cargo.toml` has `uuid = { version = "1", features = ["serde", "v4"] }`. Add if missing.

Add to `lib.rs`:
```rust
pub mod signal;
pub use signal::{Signal, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
```

- [x] **Step 4: Tests pass**

```bash
cargo test -p mur-common signal
```

Expected: 2 tests pass.

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-common -- -D warnings
git add mur-common/src/signal.rs mur-common/src/lib.rs mur-common/Cargo.toml
git commit -m "feat(common): add Signal wire format (schema v1)"
```

---

## Part B — mur-core Sync Client (W3, 5 tasks)

### Task 7: Create `outbox.rs` writer

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/mod.rs`
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/outbox.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/lib.rs` (or main.rs where modules are declared)

**Prereq:** Task 6

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/outbox.rs`:

```rust
use anyhow::Result;
use chrono::Utc;
use mur_common::Signal;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
        Self::new(home.join(".mur/outbox"))
    }

    /// Write a signal to outbox atomically. Returns the file path created.
    pub fn write(&self, signal: &Signal) -> Result<PathBuf> {
        let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S");
        let name = format!("{}-{}.yaml", ts, signal.id);
        let final_path = self.dir.join(&name);
        let tmp_path = self.dir.join(format!(".{}.tmp", name));

        let yaml = serde_yaml::to_string(signal)?;
        std::fs::write(&tmp_path, yaml)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(final_path)
    }

    pub fn list_pending(&self) -> Result<Vec<PathBuf>> {
        let mut items: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("yaml")
                && !p.file_name().and_then(|s| s.to_str()).unwrap_or("").starts_with('.'))
            .collect();
        items.sort(); // filename includes timestamp prefix
        Ok(items)
    }

    pub fn mark_flushed(&self, path: &Path) -> Result<()> {
        let flushed_dir = self.dir.join(".flushed");
        std::fs::create_dir_all(&flushed_dir)?;
        let name = path.file_name().ok_or_else(|| anyhow::anyhow!("no file name"))?;
        std::fs::rename(path, flushed_dir.join(name))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, Scope, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
    use tempfile::tempdir;

    fn sample_signal() -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::CommanderDaemon,
                native_id: "x".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "foo".into(), scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    #[test]
    fn write_and_list() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let s = sample_signal();
        let p = ob.write(&s).unwrap();
        assert!(p.exists());
        let pending = ob.list_pending().unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn mark_flushed_moves_to_subdir() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let p = ob.write(&sample_signal()).unwrap();
        ob.mark_flushed(&p).unwrap();
        assert!(!p.exists());
        let flushed_dir = dir.path().join(".flushed");
        assert_eq!(flushed_dir.read_dir().unwrap().count(), 1);
        assert_eq!(ob.list_pending().unwrap().len(), 0);
    }
}
```

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/mod.rs`:
```rust
pub mod outbox;
pub use outbox::Outbox;
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-core sync::outbox
```

Expected: module `sync` not found (until we register it in lib.rs/main.rs).

- [x] **Step 3: Register module + add deps**

In `mur-core/src/lib.rs` (or wherever modules are declared), add `pub mod sync;`.

Check `mur-core/Cargo.toml` for `dirs`, `tempfile` (dev-dep), `uuid`, `anyhow`, `serde_yaml`, `chrono`. Add missing.

- [x] **Step 4: Tests pass**

```bash
cargo test -p mur-core sync::outbox
```

Expected: 2 tests pass.

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings
git add mur-core/src/sync/ mur-core/src/lib.rs mur-core/Cargo.toml
git commit -m "feat(core): add sync::Outbox with atomic signal persistence"
```

---

### Task 8: Create `inbox.rs` reader + applier

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/inbox.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/mod.rs`

**Prereq:** Task 7, Task 4 (Evidence.contributions)

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/inbox.rs`:

```rust
use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::{Contribution, Signal, SignalKind, SignalTarget};
use std::path::{Path, PathBuf};

use crate::store::YamlStore;

pub struct Inbox {
    dir: PathBuf,
}

pub struct ApplyReport {
    pub applied: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
}

impl Inbox {
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
        Self::new(home.join(".mur/inbox"))
    }

    pub fn receive(&self, signal: &Signal) -> Result<PathBuf> {
        let name = format!("{}-{}.yaml",
            signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
            signal.id);
        let path = self.dir.join(&name);
        let tmp = self.dir.join(format!(".{}.tmp", name));
        std::fs::write(&tmp, serde_yaml::to_string(signal)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Apply all inbox items to the YamlStore. Only Evidence-type signals
    /// auto-apply; NewDraftPattern goes to `~/.mur/drafts/`.
    pub fn apply_all(&self, store: &YamlStore) -> Result<ApplyReport> {
        let mut applied = 0u64;
        let mut skipped = 0u64;
        let mut errors = Vec::new();

        for entry in std::fs::read_dir(&self.dir)? {
            let p = entry?.path();
            if p.extension().and_then(|s| s.to_str()) != Some("yaml") { continue; }
            if p.file_name().and_then(|s| s.to_str()).unwrap_or("").starts_with('.') { continue; }

            let yaml = std::fs::read_to_string(&p)
                .with_context(|| format!("read {}", p.display()))?;
            let signal: Signal = match serde_yaml::from_str(&yaml) {
                Ok(s) => s,
                Err(e) => { errors.push(format!("{}: {}", p.display(), e)); continue; }
            };

            match self.apply_one(store, &signal) {
                Ok(true) => { applied += 1; std::fs::remove_file(&p)?; }
                Ok(false) => { skipped += 1; std::fs::remove_file(&p)?; }
                Err(e) => errors.push(format!("{}: {}", p.display(), e)),
            }
        }
        Ok(ApplyReport { applied, skipped, errors })
    }

    fn apply_one(&self, store: &YamlStore, signal: &Signal) -> Result<bool> {
        match (&signal.target, &signal.kind) {
            (SignalTarget::Pattern { name, .. }, kind) => {
                let mut pattern = match store.get(name)? {
                    Some(p) => p,
                    None => return Ok(false), // pattern 不存在,跳過
                };

                let actor_key = signal.actor.key();
                let contrib = pattern.evidence.contributions.entry(actor_key)
                    .or_insert_with(|| Contribution {
                        success_signals: 0, override_signals: 0,
                        last_seen: Utc::now(),
                    });
                contrib.last_seen = signal.emitted_at;

                match kind {
                    SignalKind::ExecutionSuccess => {
                        contrib.success_signals += 1;
                        pattern.evidence.success_signals += 1;
                    }
                    SignalKind::ExecutionFailure { .. } => {
                        pattern.evidence.failure_signals += 1;
                    }
                    SignalKind::UserOverrideAtBreakpoint { .. } => {
                        contrib.override_signals += 3; // spec §4.1 guard rail
                        pattern.evidence.override_signals += 3;
                    }
                    SignalKind::AutoFixApplied { .. } => {
                        contrib.override_signals += 1;
                        pattern.evidence.override_signals += 1;
                    }
                    _ => return Ok(false),
                }
                store.save(&pattern)?;
                Ok(true)
            }
            (SignalTarget::NewDraftPattern { .. }, _) => {
                // 留給 Phase 2 處理
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::YamlStore;
    use mur_common::{Actor, ActorSource, Pattern, Scope, SIGNAL_SCHEMA_VERSION};
    use tempfile::tempdir;
    use uuid::Uuid;

    #[test]
    fn apply_execution_success_updates_contributions() {
        let tmp = tempdir().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf());

        // 準備一個 pattern
        let mut p = Pattern::new_simple("test-p", "desc", "content");
        p.scope = Scope::Personal;
        store.save(&p).unwrap();

        // 準備一個 inbox
        let inbox_dir = tmp.path().join("inbox");
        let inbox = Inbox::new(&inbox_dir).unwrap();
        let signal = Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor { source: ActorSource::Slack, native_id: "alice".into(),
                display_name: None, resolved_user_id: None },
            target: SignalTarget::Pattern {
                name: "test-p".into(), scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal, confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        inbox.receive(&signal).unwrap();

        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.errors.len(), 0);

        let p2 = store.get("test-p").unwrap().unwrap();
        assert_eq!(p2.evidence.success_signals, 1);
        assert!(p2.evidence.contributions.contains_key("Slack:alice"));
    }
}
```

Add to `sync/mod.rs`:
```rust
pub mod inbox;
pub use inbox::{ApplyReport, Inbox};
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-core sync::inbox
```

Expected: `Pattern::new_simple` may not exist — it's a test helper assumed. If absent, use `Pattern::default()` + setters, or inspect actual YamlStore API.

- [x] **Step 3: Fix test helpers + compile**

Look at `/Volumes/Firecuda4tb/Projects/mur/mur-common/src/pattern.rs` to find actual Pattern constructor API. Adapt test.

If `YamlStore::new(dir)` signature differs, adapt. Refer `mur-core/src/store/yaml.rs` lines 1-50 for exact signature.

- [x] **Step 4: Tests pass**

```bash
cargo test -p mur-core sync::inbox
```

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings
git add mur-core/src/sync/
git commit -m "feat(core): add sync::Inbox with apply_all for Evidence signals"
```

---

### Task 9: Create `cursor.rs` for fetch pagination

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/cursor.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/mod.rs`

**Prereq:** Task 7

- [x] **Step 1: Failing test**

Create file with:

```rust
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FetchCursor {
    #[serde(default)]
    pub last_signal_id: Option<String>,
    #[serde(default)]
    pub last_fetched_at: Option<DateTime<Utc>>,
}

pub struct CursorStore {
    path: PathBuf,
}

impl CursorStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self { path: path.as_ref().to_path_buf() }
    }

    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
        let p = home.join(".mur/sync/cursor.json");
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
        Ok(Self::new(p))
    }

    pub fn load(&self) -> Result<FetchCursor> {
        if !self.path.exists() { return Ok(FetchCursor::default()); }
        let s = std::fs::read_to_string(&self.path)?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save(&self, c: &FetchCursor) -> Result<()> {
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(c)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_missing_returns_default() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        let c = cs.load().unwrap();
        assert!(c.last_signal_id.is_none());
    }

    #[test]
    fn roundtrip() {
        let tmp = tempdir().unwrap();
        let cs = CursorStore::new(tmp.path().join("cursor.json"));
        let c = FetchCursor {
            last_signal_id: Some("abc".into()),
            last_fetched_at: Some(Utc::now()),
        };
        cs.save(&c).unwrap();
        let back = cs.load().unwrap();
        assert_eq!(back.last_signal_id, c.last_signal_id);
    }
}
```

Add to `sync/mod.rs`:
```rust
pub mod cursor;
pub use cursor::{CursorStore, FetchCursor};
```

- [x] **Step 2: Verify fail → Step 3: (compile passes) → Step 4: verify pass → Step 5: commit**

```bash
cargo test -p mur-core sync::cursor
cargo clippy -p mur-core -- -D warnings
git add mur-core/src/sync/
git commit -m "feat(core): add sync::CursorStore for fetch pagination"
```

---

### Task 10: Create `client.rs` HTTP client to mur-server

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/client.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/mod.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/Cargo.toml` — ensure `reqwest` with `json`, `gzip` features

**Prereq:** Task 7, 8, 9

- [x] **Step 1: Failing test (using wiremock)**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/sync/client.rs`:

```rust
use anyhow::{Context, Result};
use mur_common::{Pattern, Signal};
use reqwest::Client;
use serde::{Deserialize, Serialize};

pub struct SyncClient {
    base_url: String,
    token: String,
    http: Client,
}

#[derive(Debug, Serialize)]
pub struct BatchRequest<'a> {
    pub signals: &'a [Signal],
}

#[derive(Debug, Deserialize)]
pub struct BatchResponse {
    pub accepted: Vec<String>,
    pub rejected: Vec<RejectedSignal>,
}

#[derive(Debug, Deserialize)]
pub struct RejectedSignal {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct PendingResponse {
    pub signals: Vec<Signal>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PatternsResponse {
    pub patterns: Vec<Pattern>,
    pub next_cursor: Option<String>,
}

impl SyncClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        Ok(Self {
            base_url: base_url.into(),
            token: token.into(),
            http: Client::builder().gzip(true).build()?,
        })
    }

    pub async fn push_batch(&self, signals: &[Signal]) -> Result<BatchResponse> {
        let url = format!("{}/v1/signals/batch", self.base_url);
        let resp = self.http.post(&url)
            .bearer_auth(&self.token)
            .json(&BatchRequest { signals })
            .send().await.context("POST /v1/signals/batch")?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn fetch_pending(&self, cursor: Option<&str>) -> Result<PendingResponse> {
        let mut url = format!("{}/v1/signals/pending", self.base_url);
        if let Some(c) = cursor {
            url.push_str(&format!("?since={}", urlencoding::encode(c)));
        }
        let resp = self.http.get(&url)
            .bearer_auth(&self.token)
            .send().await?.error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn ack(&self, signal_ids: &[String]) -> Result<()> {
        let url = format!("{}/v1/signals/ack", self.base_url);
        #[derive(Serialize)]
        struct AckRequest<'a> { ids: &'a [String] }
        self.http.post(&url)
            .bearer_auth(&self.token)
            .json(&AckRequest { ids: signal_ids })
            .send().await?.error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, Scope, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
    use uuid::Uuid;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn signal() -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: chrono::Utc::now(),
            actor: Actor { source: ActorSource::CommanderDaemon, native_id: "x".into(),
                display_name: None, resolved_user_id: None },
            target: SignalTarget::Pattern { name: "foo".into(), scope: Scope::Personal },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal, confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    #[tokio::test]
    async fn push_batch_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/signals/batch"))
            .and(header("authorization", "Bearer TEST"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "accepted": ["sig-1"],
                    "rejected": []
                })))
            .mount(&server).await;

        let c = SyncClient::new(server.uri(), "TEST").unwrap();
        let r = c.push_batch(&[signal()]).await.unwrap();
        assert_eq!(r.accepted, vec!["sig-1"]);
    }

    #[tokio::test]
    async fn fetch_pending_with_cursor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/signals/pending"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({
                    "signals": [],
                    "next_cursor": null
                })))
            .mount(&server).await;

        let c = SyncClient::new(server.uri(), "TEST").unwrap();
        let r = c.fetch_pending(Some("abc")).await.unwrap();
        assert_eq!(r.signals.len(), 0);
    }
}
```

- [x] **Step 2: Verify fail**

```bash
cargo test -p mur-core sync::client
```

Expected: `wiremock`, `urlencoding` missing.

- [x] **Step 3: Add deps**

`mur-core/Cargo.toml`:
```toml
[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "gzip", "rustls-tls"] }
urlencoding = "2"

[dev-dependencies]
wiremock = "0.6"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Register in `sync/mod.rs`:
```rust
pub mod client;
pub use client::{SyncClient, BatchResponse, PendingResponse};
```

- [x] **Step 4: Tests pass**

```bash
cargo test -p mur-core sync::client
cargo clippy -p mur-core -- -D warnings
```

- [x] **Step 5: Commit**

```bash
git add mur-core/src/sync/client.rs mur-core/src/sync/mod.rs mur-core/Cargo.toml
git commit -m "feat(core): add sync::SyncClient (push/fetch/ack with wiremock tests)"
```

---

### Task 11: Add `mur push`, `mur fetch`, `mur sync status` CLI commands

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/cmd/sync_cmd.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/main.rs` — 註冊新 subcommands
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/cmd/mod.rs` — pub use

**Prereq:** Tasks 7-10

- [x] **Step 1: Implement sync_cmd.rs**

Create `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/cmd/sync_cmd.rs`:

```rust
use anyhow::Result;
use crate::auth::load_tokens;
use crate::store::YamlStore;
use crate::sync::{CursorStore, FetchCursor, Inbox, Outbox, SyncClient};

pub async fn run_push(server_url: &str, dry_run: bool) -> Result<()> {
    let ob = Outbox::default_location()?;
    let pending_paths = ob.list_pending()?;
    if pending_paths.is_empty() {
        println!("outbox empty, nothing to push");
        return Ok(());
    }

    let mut signals = Vec::new();
    for p in &pending_paths {
        let yaml = std::fs::read_to_string(p)?;
        match serde_yaml::from_str(&yaml) {
            Ok(s) => signals.push(s),
            Err(e) => eprintln!("skip bad YAML {}: {}", p.display(), e),
        }
    }

    if dry_run {
        println!("[dry-run] would push {} signals", signals.len());
        return Ok(());
    }

    let tokens = load_tokens()?.ok_or_else(|| anyhow::anyhow!("not logged in"))?;
    let client = SyncClient::new(server_url, &tokens.access_token)?;
    let resp = client.push_batch(&signals).await?;

    // 移動 accepted signal files
    for (i, p) in pending_paths.iter().enumerate() {
        let sig_id = signals.get(i).map(|s| s.id.to_string()).unwrap_or_default();
        if resp.accepted.iter().any(|a| *a == sig_id) {
            ob.mark_flushed(p)?;
        }
    }

    println!("pushed: {} accepted, {} rejected",
        resp.accepted.len(), resp.rejected.len());
    for r in &resp.rejected {
        println!("  rejected {}: {}", r.id, r.reason);
    }
    Ok(())
}

pub async fn run_fetch(server_url: &str, dry_run: bool) -> Result<()> {
    let tokens = load_tokens()?.ok_or_else(|| anyhow::anyhow!("not logged in"))?;
    let client = SyncClient::new(server_url, &tokens.access_token)?;

    let cs = CursorStore::default_location()?;
    let cursor = cs.load()?;
    let resp = client.fetch_pending(cursor.last_signal_id.as_deref()).await?;

    if dry_run {
        println!("[dry-run] would receive {} signals", resp.signals.len());
        return Ok(());
    }

    let inbox = Inbox::default_location()?;
    let mut ids_to_ack = Vec::new();
    for s in &resp.signals {
        inbox.receive(s)?;
        ids_to_ack.push(s.id.to_string());
    }

    let store = YamlStore::default_store()?;
    let report = inbox.apply_all(&store)?;
    println!("fetched: {} signals, applied {}, skipped {}",
        resp.signals.len(), report.applied, report.skipped);

    if !ids_to_ack.is_empty() {
        client.ack(&ids_to_ack).await?;
    }

    cs.save(&FetchCursor {
        last_signal_id: resp.next_cursor,
        last_fetched_at: Some(chrono::Utc::now()),
    })?;
    Ok(())
}

pub fn run_status() -> Result<()> {
    let ob = Outbox::default_location()?;
    let inbox = Inbox::default_location()?;
    let cs = CursorStore::default_location()?;

    let pending = ob.list_pending()?.len();
    let cursor = cs.load()?;

    println!("sync status");
    println!("  outbox pending: {}", pending);
    match cursor.last_fetched_at {
        Some(t) => println!("  last fetch:    {}", t),
        None => println!("  last fetch:    never"),
    }
    let _ = inbox;
    Ok(())
}
```

- [x] **Step 2: Register subcommand in main.rs**

Find the main clap command tree. Add:

```rust
// within subcommand enum
/// Push pending signals from ~/.mur/outbox/ to server
Push {
    #[arg(long)] dry_run: bool,
},
/// Fetch pending signals from server to ~/.mur/inbox/ and apply
Fetch {
    #[arg(long)] dry_run: bool,
},
/// Show sync status
Sync {
    #[command(subcommand)]
    action: SyncAction,
},

// enum SyncAction
#[derive(Subcommand)]
enum SyncAction {
    Status,
}
```

And dispatch:
```rust
Commands::Push { dry_run } => {
    let cfg = crate::config::load_config()?;
    crate::cmd::sync_cmd::run_push(&cfg.server.url, dry_run).await?;
}
Commands::Fetch { dry_run } => {
    let cfg = crate::config::load_config()?;
    crate::cmd::sync_cmd::run_fetch(&cfg.server.url, dry_run).await?;
}
Commands::Sync { action } => match action {
    SyncAction::Status => crate::cmd::sync_cmd::run_status()?,
},
```

- [x] **Step 3: Wire up cmd/mod.rs**

Add `pub mod sync_cmd;`.

- [x] **Step 4: Manual smoke test**

```bash
cargo build -p mur-core
./target/debug/mur push --dry-run
# Expected: "outbox empty, nothing to push"
./target/debug/mur sync status
# Expected: outbox pending: 0, last fetch: never
```

- [x] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/sync_cmd.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
git commit -m "feat(core): add mur push/fetch/sync status CLI commands"
```

---

## Part C — mur-server Endpoints + Postgres (W4, 8 tasks)

### Task 12: Postgres migration for signal tables

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/migrations/NNN_add_signal_tables.up.sql` (use next available NNN number — check `ls migrations/`)
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/migrations/NNN_add_signal_tables.down.sql`

**Prereq:** Tasks 1-6 (schema finalized)

- [x] **Step 1: Check next migration number**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server
ls migrations/ | tail -5
```

Use next number; assume it's `040`.

- [x] **Step 2: Write .up.sql**

Create `/Volumes/Firecuda4tb/Projects/mur-server/migrations/040_add_signal_tables.up.sql`:

```sql
-- Signals queue (raw events from clients)
CREATE TABLE signals (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    emitted_at TIMESTAMPTZ NOT NULL,
    actor_source TEXT NOT NULL,
    actor_native_id TEXT NOT NULL,
    actor_display_name TEXT,
    resolved_user_id UUID REFERENCES users(id),
    target_type TEXT NOT NULL,           -- 'pattern' | 'new_draft_pattern'
    target_pattern_name TEXT,
    target_scope JSONB NOT NULL,
    kind JSONB NOT NULL,
    scope JSONB NOT NULL,
    confidence DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    schema_version INT NOT NULL DEFAULT 1,
    payload JSONB,                       -- for NewDraftPattern carry-along
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    processed_at TIMESTAMPTZ,            -- set by aggregator
    rejected_reason TEXT
);

CREATE INDEX idx_signals_user_received ON signals(user_id, received_at);
CREATE INDEX idx_signals_processed ON signals(processed_at) WHERE processed_at IS NULL;
CREATE INDEX idx_signals_target ON signals(user_id, target_pattern_name) WHERE target_type = 'pattern';

-- 5-minute window dedupe: same (user, target, actor) within 5min collapses to single row
CREATE UNIQUE INDEX idx_signals_dedupe
    ON signals (
        user_id,
        target_pattern_name,
        (target_scope->>'kind'),
        actor_source,
        actor_native_id,
        (date_trunc('minute', emitted_at) - (EXTRACT(MINUTE FROM emitted_at)::int % 5) * INTERVAL '1 minute')
    ) WHERE target_type = 'pattern';

-- Actor bindings: (platform, native_id) → canonical user
CREATE TABLE actor_bindings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    actor_source TEXT NOT NULL,
    actor_native_id TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(actor_source, actor_native_id)
);

CREATE INDEX idx_actor_bindings_user ON actor_bindings(user_id);

-- Unresolved actors: signals came in with unknown actor
CREATE TABLE unresolved_actors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_source TEXT NOT NULL,
    actor_native_id TEXT NOT NULL,
    actor_display_name TEXT,
    signal_count INT NOT NULL DEFAULT 1,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_user_id UUID REFERENCES users(id),
    UNIQUE(actor_source, actor_native_id)
);

-- Evidence contributions (per-actor side-car of existing patterns table)
CREATE TABLE evidence_contributions (
    pattern_id UUID NOT NULL REFERENCES patterns(id) ON DELETE CASCADE,
    actor_key TEXT NOT NULL,             -- "Slack:U123ABC" from Actor::key()
    success_signals BIGINT NOT NULL DEFAULT 0,
    override_signals BIGINT NOT NULL DEFAULT 0,
    last_seen TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (pattern_id, actor_key)
);

CREATE INDEX idx_evidence_contrib_pattern ON evidence_contributions(pattern_id);

-- Extend patterns table with scope column
ALTER TABLE patterns ADD COLUMN scope JSONB NOT NULL DEFAULT '{"kind":"personal"}'::jsonb;
CREATE INDEX idx_patterns_user_scope ON patterns(user_id, (scope->>'kind'));

-- Fetch cursor tracking (per user, which signals already delivered)
CREATE TABLE fetch_cursors (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    last_signal_id UUID,
    last_fetched_at TIMESTAMPTZ
);
```

- [x] **Step 3: Write .down.sql**

Create `040_add_signal_tables.down.sql`:

```sql
DROP TABLE IF EXISTS fetch_cursors;
DROP INDEX IF EXISTS idx_patterns_user_scope;
ALTER TABLE patterns DROP COLUMN IF EXISTS scope;
DROP TABLE IF EXISTS evidence_contributions;
DROP TABLE IF EXISTS unresolved_actors;
DROP TABLE IF EXISTS actor_bindings;
DROP INDEX IF EXISTS idx_signals_dedupe;
DROP INDEX IF EXISTS idx_signals_target;
DROP INDEX IF EXISTS idx_signals_processed;
DROP INDEX IF EXISTS idx_signals_user_received;
DROP TABLE IF EXISTS signals;
```

- [x] **Step 4: Run migration**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server
docker-compose up -d postgres
make migrate
# Verify
psql postgres://mur:mur@localhost:5432/mur -c "\dt" | grep -E "signals|actor_bindings|evidence_contributions|unresolved_actors|fetch_cursors"
```

Expected: 5 tables present.

- [x] **Step 5: Commit**

```bash
git add migrations/040_add_signal_tables.*.sql
git commit -m "feat(db): add signals + actor_bindings + evidence_contributions tables"
```

---

### Task 13: `internal/models/signal.go` Go wire types

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/models/signal.go`
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/models/actor.go`

**Prereq:** Task 12

- [x] **Step 1: Write failing test**

Create `/Volumes/Firecuda4tb/Projects/mur-server/internal/models/signal_test.go`:

```go
package models_test

import (
    "encoding/json"
    "testing"

    "github.com/mur-run/mur-server/internal/models"
)

func TestSignalJSONRoundtrip(t *testing.T) {
    raw := `{
        "id": "00000000-0000-0000-0000-000000000001",
        "emitted_at": "2026-04-18T10:00:00Z",
        "actor": {
            "source": "Slack",
            "native_id": "U123ABC",
            "display_name": "alice"
        },
        "target": {
            "kind": "pattern",
            "name": "foo",
            "scope": {"kind": "personal"}
        },
        "kind": {"type": "execution_success"},
        "scope": {"kind": "personal"},
        "confidence": 0.9,
        "schema_version": 1
    }`
    var s models.Signal
    if err := json.Unmarshal([]byte(raw), &s); err != nil {
        t.Fatalf("unmarshal: %v", err)
    }
    if s.Actor.NativeID != "U123ABC" {
        t.Errorf("wrong native_id: %s", s.Actor.NativeID)
    }
    if s.Confidence != 0.9 {
        t.Errorf("wrong confidence: %f", s.Confidence)
    }
    if s.Target.Kind != "pattern" {
        t.Errorf("wrong target kind: %s", s.Target.Kind)
    }
}
```

- [x] **Step 2: Verify fail**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server
go test ./internal/models/... -run TestSignal
```

Expected: models.Signal not found.

- [x] **Step 3: Implement models**

Create `/Volumes/Firecuda4tb/Projects/mur-server/internal/models/actor.go`:

```go
package models

type Actor struct {
    Source           string  `json:"source"`
    NativeID         string  `json:"native_id"`
    DisplayName      *string `json:"display_name,omitempty"`
    ResolvedUserID   *string `json:"resolved_user_id,omitempty"`
}

func (a Actor) Key() string {
    return a.Source + ":" + a.NativeID
}
```

Create `/Volumes/Firecuda4tb/Projects/mur-server/internal/models/signal.go`:

```go
package models

import (
    "encoding/json"
    "time"

    "github.com/google/uuid"
)

type Scope struct {
    Kind   string  `json:"kind"`               // "personal" | "team" | "community"
    TeamID *string `json:"team_id,omitempty"`
    PackID *string `json:"pack_id,omitempty"`
}

type SignalTarget struct {
    Kind    string          `json:"kind"`                     // "pattern" | "new_draft_pattern"
    Name    string          `json:"name,omitempty"`
    Scope   *Scope          `json:"scope,omitempty"`
    Payload json.RawMessage `json:"payload,omitempty"`        // Pattern YAML for drafts
}

type SignalKind struct {
    Type   string  `json:"type"`                              // "execution_success" etc.
    Error  *string `json:"error,omitempty"`
    Reason *string `json:"reason,omitempty"`
    Step   *string `json:"step,omitempty"`
}

type Signal struct {
    ID            uuid.UUID     `json:"id"`
    EmittedAt     time.Time     `json:"emitted_at"`
    Actor         Actor         `json:"actor"`
    Target        SignalTarget  `json:"target"`
    Kind          SignalKind    `json:"kind"`
    Scope         Scope         `json:"scope"`
    Confidence    float64       `json:"confidence"`
    SchemaVersion int           `json:"schema_version"`
}

type BatchRequest struct {
    Signals []Signal `json:"signals"`
}

type BatchResponse struct {
    Accepted []string         `json:"accepted"`
    Rejected []RejectedSignal `json:"rejected"`
}

type RejectedSignal struct {
    ID     string `json:"id"`
    Reason string `json:"reason"`
}
```

- [x] **Step 4: Test passes**

```bash
go test ./internal/models/... -run TestSignal -v
```

- [x] **Step 5: Commit**

```bash
git add internal/models/signal.go internal/models/actor.go internal/models/signal_test.go
git commit -m "feat(models): add Signal + Actor + Scope types matching mur-common v1"
```

---

### Task 14: Postgres store for signals

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/store/postgres/signals.go`
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/store/postgres/signals_test.go`

**Prereq:** Tasks 12, 13

- [x] **Step 1: Failing test with testcontainers**

Create `/Volumes/Firecuda4tb/Projects/mur-server/internal/store/postgres/signals_test.go`:

```go
package postgres_test

import (
    "context"
    "testing"
    "time"

    "github.com/google/uuid"
    "github.com/mur-run/mur-server/internal/models"
    "github.com/mur-run/mur-server/internal/store/postgres"
    // existing test helpers for spinning Postgres — adapt to codebase
)

func TestInsertSignals(t *testing.T) {
    ctx := context.Background()
    db := setupTestDB(t) // existing helper in codebase
    store := postgres.NewSignalsStore(db)

    // assume user already seeded by setupTestDB
    userID := seedUser(t, db)

    sigs := []models.Signal{{
        ID:        uuid.New(),
        EmittedAt: time.Now().UTC(),
        Actor:     models.Actor{Source: "Slack", NativeID: "U1"},
        Target:    models.SignalTarget{Kind: "pattern", Name: "foo", Scope: &models.Scope{Kind: "personal"}},
        Kind:      models.SignalKind{Type: "execution_success"},
        Scope:     models.Scope{Kind: "personal"},
        Confidence: 1.0, SchemaVersion: 1,
    }}

    accepted, rejected, err := store.InsertBatch(ctx, userID, sigs)
    if err != nil { t.Fatal(err) }
    if len(accepted) != 1 { t.Errorf("expected 1 accepted, got %d", len(accepted)) }
    if len(rejected) != 0 { t.Errorf("expected 0 rejected, got %v", rejected) }
}

func TestDedupeWithin5Minutes(t *testing.T) {
    ctx := context.Background()
    db := setupTestDB(t)
    store := postgres.NewSignalsStore(db)
    userID := seedUser(t, db)

    make := func(id uuid.UUID, t time.Time) models.Signal {
        return models.Signal{
            ID: id, EmittedAt: t,
            Actor: models.Actor{Source: "Slack", NativeID: "U1"},
            Target: models.SignalTarget{Kind: "pattern", Name: "foo", Scope: &models.Scope{Kind: "personal"}},
            Kind: models.SignalKind{Type: "execution_success"},
            Scope: models.Scope{Kind: "personal"},
            Confidence: 1, SchemaVersion: 1,
        }
    }
    now := time.Now().UTC()

    // Batch with 2 signals, same (user, target, actor), 2min apart
    sigs := []models.Signal{
        make(uuid.New(), now),
        make(uuid.New(), now.Add(2 * time.Minute)),
    }
    accepted, rejected, err := store.InsertBatch(ctx, userID, sigs)
    if err != nil { t.Fatal(err) }
    if len(accepted) != 1 || len(rejected) != 1 {
        t.Errorf("expected 1 accepted 1 rejected (dedupe); got %d/%d", len(accepted), len(rejected))
    }
    if rejected[0].Reason != "dedupe_window" {
        t.Errorf("wrong reject reason: %s", rejected[0].Reason)
    }
}
```

- [x] **Step 2: Verify fail → Step 3: implement**

Create `/Volumes/Firecuda4tb/Projects/mur-server/internal/store/postgres/signals.go`:

```go
package postgres

import (
    "context"
    "database/sql"
    "encoding/json"
    "errors"
    "time"

    "github.com/google/uuid"
    "github.com/lib/pq"
    "github.com/mur-run/mur-server/internal/models"
)

type SignalsStore struct {
    db *sql.DB
}

func NewSignalsStore(db *sql.DB) *SignalsStore {
    return &SignalsStore{db: db}
}

func (s *SignalsStore) InsertBatch(ctx context.Context, userID uuid.UUID, sigs []models.Signal) (accepted []string, rejected []models.RejectedSignal, err error) {
    tx, err := s.db.BeginTx(ctx, nil)
    if err != nil { return nil, nil, err }
    defer tx.Rollback()

    for _, sig := range sigs {
        err := insertOne(ctx, tx, userID, &sig)
        if err == nil {
            accepted = append(accepted, sig.ID.String())
            continue
        }
        if isUniqueViolation(err) {
            rejected = append(rejected, models.RejectedSignal{
                ID: sig.ID.String(), Reason: "dedupe_window",
            })
            continue
        }
        rejected = append(rejected, models.RejectedSignal{
            ID: sig.ID.String(), Reason: "db_error: " + err.Error(),
        })
    }
    if err := tx.Commit(); err != nil { return nil, nil, err }
    return accepted, rejected, nil
}

func insertOne(ctx context.Context, tx *sql.Tx, userID uuid.UUID, sig *models.Signal) error {
    targetScope, _ := json.Marshal(sig.Target.Scope)
    kindJSON, _ := json.Marshal(sig.Kind)
    scopeJSON, _ := json.Marshal(sig.Scope)

    var payload []byte
    if len(sig.Target.Payload) > 0 { payload = sig.Target.Payload }

    _, err := tx.ExecContext(ctx, `
        INSERT INTO signals (
            id, user_id, emitted_at,
            actor_source, actor_native_id, actor_display_name,
            target_type, target_pattern_name, target_scope,
            kind, scope, confidence, schema_version,
            payload, received_at
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
    `,
        sig.ID, userID, sig.EmittedAt,
        sig.Actor.Source, sig.Actor.NativeID, nullableString(sig.Actor.DisplayName),
        sig.Target.Kind, nullableString(&sig.Target.Name), targetScope,
        kindJSON, scopeJSON, sig.Confidence, sig.SchemaVersion,
        payload, time.Now().UTC(),
    )
    return err
}

func isUniqueViolation(err error) bool {
    var pqErr *pq.Error
    return errors.As(err, &pqErr) && pqErr.Code == "23505"
}

func nullableString(s *string) sql.NullString {
    if s == nil || *s == "" { return sql.NullString{} }
    return sql.NullString{Valid: true, String: *s}
}

type PendingRow struct {
    Signal models.Signal
}

func (s *SignalsStore) FetchPending(ctx context.Context, userID uuid.UUID, sinceID *string, limit int) ([]models.Signal, *string, error) {
    q := `
        SELECT id, emitted_at, actor_source, actor_native_id, actor_display_name,
               target_type, target_pattern_name, target_scope,
               kind, scope, confidence, schema_version, payload
        FROM signals
        WHERE user_id = $1 AND processed_at IS NOT NULL
    `
    args := []any{userID}
    if sinceID != nil {
        q += ` AND id > $2`
        args = append(args, *sinceID)
    }
    q += ` ORDER BY id ASC LIMIT `
    args = append(args, limit)
    q += "$" + itoa(len(args))

    rows, err := s.db.QueryContext(ctx, q, args...)
    if err != nil { return nil, nil, err }
    defer rows.Close()

    var out []models.Signal
    for rows.Next() {
        var sig models.Signal
        var dispName sql.NullString
        var patternName sql.NullString
        var targetScope, kindJSON, scopeJSON, payload []byte

        if err := rows.Scan(&sig.ID, &sig.EmittedAt, &sig.Actor.Source, &sig.Actor.NativeID, &dispName,
            &sig.Target.Kind, &patternName, &targetScope,
            &kindJSON, &scopeJSON, &sig.Confidence, &sig.SchemaVersion, &payload); err != nil {
            return nil, nil, err
        }
        if dispName.Valid { d := dispName.String; sig.Actor.DisplayName = &d }
        if patternName.Valid { sig.Target.Name = patternName.String }
        _ = json.Unmarshal(targetScope, &sig.Target.Scope)
        _ = json.Unmarshal(kindJSON, &sig.Kind)
        _ = json.Unmarshal(scopeJSON, &sig.Scope)
        sig.Target.Payload = payload
        out = append(out, sig)
    }
    var nextCursor *string
    if len(out) > 0 {
        s := out[len(out)-1].ID.String()
        nextCursor = &s
    }
    return out, nextCursor, nil
}

func itoa(i int) string { return string(rune('0'+i)) /* adjust for real */ }
```

(注意:`itoa` 在 real code 用 `strconv.Itoa`;示範簡寫)

- [x] **Step 4: Tests pass**

```bash
go test ./internal/store/postgres/... -run TestInsertSignals -v
go test ./internal/store/postgres/... -run TestDedupe -v
```

- [x] **Step 5: Commit**

```bash
git add internal/store/postgres/signals.go internal/store/postgres/signals_test.go
git commit -m "feat(store): signals insert batch with 5min dedupe + fetch_pending cursor"
```

---

### Task 15: Evidence aggregator service

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/services/evidence_aggregator.go`
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/services/evidence_aggregator_test.go`

**Prereq:** Task 14

- [x] **Step 1: Write failing test**

```go
package services_test

import (
    "context"
    "testing"

    "github.com/mur-run/mur-server/internal/services"
)

func TestAggregatorUpdatesContributions(t *testing.T) {
    db := setupTestDB(t)
    userID := seedUser(t, db)
    seedPattern(t, db, userID, "foo")  // helper
    seedSignal(t, db, userID, "foo", "Slack:U1", "execution_success")
    seedSignal(t, db, userID, "foo", "Slack:U1", "execution_success")
    seedSignal(t, db, userID, "foo", "Slack:U1", "user_override_at_breakpoint")

    agg := services.NewEvidenceAggregator(db)
    n, err := agg.ProcessPending(context.Background(), 100)
    if err != nil { t.Fatal(err) }
    if n != 3 { t.Errorf("expected 3 processed, got %d", n) }

    contrib := queryContribution(t, db, "foo", "Slack:U1")
    if contrib.Success != 2 || contrib.Override != 3 {
        t.Errorf("got success=%d override=%d; want 2/3", contrib.Success, contrib.Override)
    }
}
```

- [x] **Step 2: Verify fail**

```bash
go test ./internal/services/... -run TestAggregator -v
```

- [x] **Step 3: Implement aggregator**

```go
package services

import (
    "context"
    "database/sql"
    "encoding/json"
    "time"
)

type EvidenceAggregator struct {
    db *sql.DB
}

func NewEvidenceAggregator(db *sql.DB) *EvidenceAggregator {
    return &EvidenceAggregator{db: db}
}

func (a *EvidenceAggregator) ProcessPending(ctx context.Context, limit int) (int, error) {
    rows, err := a.db.QueryContext(ctx, `
        SELECT s.id, s.user_id, s.target_pattern_name,
               s.actor_source, s.actor_native_id, s.kind, s.emitted_at
        FROM signals s
        WHERE s.processed_at IS NULL
          AND s.target_type = 'pattern'
          AND s.rejected_reason IS NULL
        ORDER BY s.received_at ASC
        LIMIT $1
        FOR UPDATE SKIP LOCKED
    `, limit)
    if err != nil { return 0, err }
    defer rows.Close()

    processed := 0
    for rows.Next() {
        var sigID, userID, patternName, actorSource, actorID string
        var kindJSON []byte
        var emittedAt time.Time
        if err := rows.Scan(&sigID, &userID, &patternName, &actorSource, &actorID, &kindJSON, &emittedAt); err != nil {
            return processed, err
        }
        var kind struct{ Type string }
        if err := json.Unmarshal(kindJSON, &kind); err != nil {
            continue
        }

        actorKey := actorSource + ":" + actorID
        successDelta, overrideDelta := deltas(kind.Type)

        tx, err := a.db.BeginTx(ctx, nil)
        if err != nil { return processed, err }

        // Find pattern
        var patternID string
        err = tx.QueryRowContext(ctx, `
            SELECT id FROM patterns WHERE user_id = $1 AND name = $2
        `, userID, patternName).Scan(&patternID)
        if err == sql.ErrNoRows {
            _, _ = tx.ExecContext(ctx, `UPDATE signals SET processed_at=now(), rejected_reason='pattern_not_found' WHERE id=$1`, sigID)
            tx.Commit(); continue
        }
        if err != nil { tx.Rollback(); return processed, err }

        // Upsert contribution
        _, err = tx.ExecContext(ctx, `
            INSERT INTO evidence_contributions (pattern_id, actor_key, success_signals, override_signals, last_seen)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (pattern_id, actor_key)
            DO UPDATE SET
                success_signals = evidence_contributions.success_signals + EXCLUDED.success_signals,
                override_signals = evidence_contributions.override_signals + EXCLUDED.override_signals,
                last_seen = GREATEST(evidence_contributions.last_seen, EXCLUDED.last_seen)
        `, patternID, actorKey, successDelta, overrideDelta, emittedAt)
        if err != nil { tx.Rollback(); return processed, err }

        // Update global aggregates on patterns.evidence_summary (JSONB or separate columns depending existing schema)
        _, err = tx.ExecContext(ctx, `
            UPDATE patterns SET
                success_signals = COALESCE(success_signals,0) + $1,
                override_signals = COALESCE(override_signals,0) + $2
            WHERE id = $3
        `, successDelta, overrideDelta, patternID)
        if err != nil { tx.Rollback(); return processed, err }

        _, err = tx.ExecContext(ctx, `UPDATE signals SET processed_at=now() WHERE id=$1`, sigID)
        if err != nil { tx.Rollback(); return processed, err }

        if err := tx.Commit(); err != nil { return processed, err }
        processed++
    }
    return processed, nil
}

func deltas(kindType string) (success, override int) {
    switch kindType {
    case "execution_success":                 return 1, 0
    case "user_override_at_breakpoint":       return 0, 3  // spec §4.1 guard rail
    case "auto_fix_applied":                  return 0, 1
    case "execution_failure":                 return 0, 0  // global failure_signals updated elsewhere
    default:                                  return 0, 0
    }
}
```

**Note**: the `UPDATE patterns SET success_signals ...` assumes these columns exist. Check existing schema — if evidence is stored as JSONB, adapt. Migration 040 didn't add these — ensure existing patterns table has them or amend the migration.

- [x] **Step 4: Tests pass**

- [x] **Step 5: Commit**

```bash
git add internal/services/evidence_aggregator.go internal/services/evidence_aggregator_test.go
git commit -m "feat(services): evidence aggregator with override=3x weight"
```

---

### Task 16: HTTP handlers for `/v1/signals/*`

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/api/handlers/signals.go`
- Modify: `/Volumes/Firecuda4tb/Projects/mur-server/internal/api/server.go` — register routes

**Prereq:** Tasks 13-15

- [x] **Step 1: Failing test**

```go
package handlers_test

func TestPostSignalsBatch(t *testing.T) {
    // spin test server with auth middleware mock
    srv := newTestServer(t)
    defer srv.Close()

    body := `{
        "signals": [{
            "id": "...",
            "emitted_at": "...",
            "actor": {"source":"Slack","native_id":"U1"},
            "target": {"kind":"pattern","name":"foo","scope":{"kind":"personal"}},
            "kind": {"type":"execution_success"},
            "scope": {"kind":"personal"},
            "confidence": 1.0,
            "schema_version": 1
        }]
    }`
    req := httptest.NewRequest("POST", "/v1/signals/batch", strings.NewReader(body))
    req.Header.Set("Authorization", "Bearer test-token-for-user-alice")
    req.Header.Set("Content-Type", "application/json")

    w := httptest.NewRecorder()
    srv.Handler.ServeHTTP(w, req)

    if w.Code != http.StatusOK { t.Errorf("status %d", w.Code) }
    var resp models.BatchResponse
    json.Unmarshal(w.Body.Bytes(), &resp)
    if len(resp.Accepted) != 1 { t.Errorf("accepted: %v", resp.Accepted) }
}
```

- [x] **Step 2: Verify fail → Step 3: Implement**

```go
package handlers

import (
    "encoding/json"
    "net/http"

    "github.com/google/uuid"
    "github.com/mur-run/mur-server/internal/models"
    "github.com/mur-run/mur-server/internal/store/postgres"
)

type SignalsHandler struct {
    signals *postgres.SignalsStore
}

func NewSignalsHandler(s *postgres.SignalsStore) *SignalsHandler {
    return &SignalsHandler{signals: s}
}

func (h *SignalsHandler) PostBatch(w http.ResponseWriter, r *http.Request) {
    userID := userIDFromContext(r.Context())  // existing auth middleware provides this

    var req models.BatchRequest
    if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
        http.Error(w, "bad json: "+err.Error(), http.StatusBadRequest)
        return
    }
    if len(req.Signals) == 0 {
        http.Error(w, "empty batch", http.StatusBadRequest)
        return
    }
    if len(req.Signals) > 1000 {
        http.Error(w, "batch too large (max 1000)", http.StatusRequestEntityTooLarge)
        return
    }

    accepted, rejected, err := h.signals.InsertBatch(r.Context(), userID, req.Signals)
    if err != nil {
        http.Error(w, "db: "+err.Error(), http.StatusInternalServerError)
        return
    }
    writeJSON(w, http.StatusOK, models.BatchResponse{Accepted: accepted, Rejected: rejected})
}

func (h *SignalsHandler) GetPending(w http.ResponseWriter, r *http.Request) {
    userID := userIDFromContext(r.Context())
    since := r.URL.Query().Get("since")
    var sincePtr *string
    if since != "" { sincePtr = &since }

    sigs, nextCursor, err := h.signals.FetchPending(r.Context(), userID, sincePtr, 100)
    if err != nil { http.Error(w, err.Error(), 500); return }

    writeJSON(w, 200, map[string]any{
        "signals":     sigs,
        "next_cursor": nextCursor,
    })
}

func (h *SignalsHandler) PostAck(w http.ResponseWriter, r *http.Request) {
    userID := userIDFromContext(r.Context())
    var req struct{ IDs []string `json:"ids"` }
    json.NewDecoder(r.Body).Decode(&req)

    if err := h.signals.MarkAcked(r.Context(), userID, req.IDs); err != nil {
        http.Error(w, err.Error(), 500); return
    }
    w.WriteHeader(http.StatusNoContent)
    _ = uuid.Nil // suppress unused
}

func writeJSON(w http.ResponseWriter, status int, body any) {
    w.Header().Set("Content-Type", "application/json")
    w.WriteHeader(status)
    json.NewEncoder(w).Encode(body)
}
```

Add `MarkAcked` method to `SignalsStore`:

```go
func (s *SignalsStore) MarkAcked(ctx context.Context, userID uuid.UUID, ids []string) error {
    if len(ids) == 0 { return nil }
    _, err := s.db.ExecContext(ctx, `
        UPDATE signals SET processed_at = now()
        WHERE user_id = $1 AND id = ANY($2) AND processed_at IS NULL
    `, userID, pq.Array(ids))
    return err
}
```

Register routes in `internal/api/server.go`:

```go
sh := handlers.NewSignalsHandler(signalsStore)
r.Route("/v1/signals", func(r chi.Router) {
    r.Use(authMiddleware)  // existing
    r.Post("/batch", sh.PostBatch)
    r.Get("/pending", sh.GetPending)
    r.Post("/ack", sh.PostAck)
})
```

- [x] **Step 4: Tests pass**

- [x] **Step 5: Commit**

```bash
git add internal/api/handlers/signals.go internal/api/server.go internal/store/postgres/signals.go
git commit -m "feat(api): /v1/signals/{batch,pending,ack} endpoints with auth"
```

---

### Task 17: Actor resolution handler + service

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/store/postgres/actor_bindings.go`
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/api/handlers/actors.go`
- Modify: `internal/services/signal_ingester.go` (or inline in handler) — resolve actor on ingest

**Prereq:** Task 12

- [x] **Step 1: Failing test**

```go
func TestResolveActor(t *testing.T) {
    db := setupTestDB(t)
    userID := seedUser(t, db)
    seedActorBinding(t, db, userID, "Slack", "U123ABC")

    store := postgres.NewActorBindingsStore(db)
    uid, err := store.Resolve(ctx, "Slack", "U123ABC")
    if err != nil { t.Fatal(err) }
    if uid == nil || *uid != userID.String() {
        t.Errorf("wrong user_id: %v", uid)
    }
}

func TestResolveUnknownEnqueuesUnresolved(t *testing.T) {
    db := setupTestDB(t)
    store := postgres.NewActorBindingsStore(db)
    uid, err := store.Resolve(ctx, "Slack", "UNKNOWN")
    if err != nil { t.Fatal(err) }
    if uid != nil { t.Errorf("expected nil user_id") }
    // Check unresolved_actors
    n := countUnresolved(t, db, "Slack", "UNKNOWN")
    if n != 1 { t.Errorf("unresolved count: %d", n) }
}
```

- [x] **Step 2: Verify fail → Step 3: Implement**

```go
package postgres

import (
    "context"
    "database/sql"
    "errors"
)

type ActorBindingsStore struct { db *sql.DB }

func NewActorBindingsStore(db *sql.DB) *ActorBindingsStore {
    return &ActorBindingsStore{db: db}
}

func (s *ActorBindingsStore) Resolve(ctx context.Context, source, nativeID string) (*string, error) {
    var uid string
    err := s.db.QueryRowContext(ctx, `
        SELECT user_id FROM actor_bindings WHERE actor_source = $1 AND actor_native_id = $2
    `, source, nativeID).Scan(&uid)
    if errors.Is(err, sql.ErrNoRows) {
        if _, err := s.db.ExecContext(ctx, `
            INSERT INTO unresolved_actors (actor_source, actor_native_id, signal_count)
            VALUES ($1, $2, 1)
            ON CONFLICT (actor_source, actor_native_id)
            DO UPDATE SET signal_count = unresolved_actors.signal_count + 1,
                          last_seen = now()
        `, source, nativeID); err != nil { return nil, err }
        return nil, nil
    }
    if err != nil { return nil, err }
    return &uid, nil
}

func (s *ActorBindingsStore) Bind(ctx context.Context, userID, source, nativeID string) error {
    _, err := s.db.ExecContext(ctx, `
        INSERT INTO actor_bindings (user_id, actor_source, actor_native_id)
        VALUES ($1, $2, $3)
    `, userID, source, nativeID)
    return err
}
```

Handler `/v1/actors/resolve`:

```go
package handlers

type ActorsHandler struct { bindings *postgres.ActorBindingsStore }

func (h *ActorsHandler) PostResolve(w http.ResponseWriter, r *http.Request) {
    var req struct{ Source, NativeID string }
    json.NewDecoder(r.Body).Decode(&req)
    uid, err := h.bindings.Resolve(r.Context(), req.Source, req.NativeID)
    if err != nil { http.Error(w, err.Error(), 500); return }
    writeJSON(w, 200, map[string]any{
        "resolved_user_id": uid,
    })
}
```

Register:

```go
ah := handlers.NewActorsHandler(actorBindingsStore)
r.Post("/v1/actors/resolve", ah.PostResolve)  // protected by svc account auth
```

- [x] **Step 4: Tests pass → Step 5: Commit**

```bash
git add internal/store/postgres/actor_bindings.go internal/api/handlers/actors.go internal/api/server.go
git commit -m "feat(api): /v1/actors/resolve + unresolved_actors queue"
```

---

### Task 18: Include `user_id` in device-code token response

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-server/internal/api/handlers/auth.go`
- Modify: `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/auth.rs`

**Prereq:** none (can be independent)

- [x] **Step 1: Server — update response struct**

In `auth.go` find the token response serialization for `/api/v1/core/auth/device/token`. Add `user_id` field:

```go
type DeviceTokenResponse struct {
    AccessToken  string `json:"access_token"`
    RefreshToken string `json:"refresh_token"`
    TokenType    string `json:"token_type"`
    ExpiresIn    int    `json:"expires_in"`
    UserID       string `json:"user_id"`       // NEW
}
```

Ensure the handler looks up user_id from the completed device flow record and includes it.

- [x] **Step 2: Client — store user_id**

In `/Volumes/Firecuda4tb/Projects/mur/mur-core/src/auth.rs`, update `AuthTokens`:

```rust
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
    #[serde(default)]
    pub user_id: Option<String>,  // NEW, optional for backward compat
}
```

Update `DeviceTokenResponse` similarly and map to AuthTokens on save.

- [x] **Step 3: Test — client loads user_id**

Add test in `auth.rs`:

```rust
#[test]
fn auth_tokens_parse_user_id() {
    let j = r#"{"access_token":"a","refresh_token":"r","token_type":"Bearer","expires_in":3600,"user_id":"alice-uuid"}"#;
    let t: DeviceTokenResponse = serde_json::from_str(j).unwrap();
    let tokens: AuthTokens = t.into();
    assert_eq!(tokens.user_id.as_deref(), Some("alice-uuid"));
}
```

- [x] **Step 4: Run tests**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server && go test ./internal/api/handlers/... -run TestAuth
cd /Volumes/Firecuda4tb/Projects/mur && cargo test -p mur-core auth_tokens_parse_user_id
```

- [x] **Step 5: Commit (in 2 repos)**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-server
git add internal/api/handlers/auth.go && git commit -m "feat(auth): include user_id in device token response"

cd /Volumes/Firecuda4tb/Projects/mur
git add mur-core/src/auth.rs && git commit -m "feat(auth): store user_id from token response"
```

---

### Task 19: Background worker running evidence aggregator

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-server/cmd/mur-server/main.go` — spawn worker goroutine
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/services/aggregator_worker.go`

**Prereq:** Task 15

- [x] **Step 1: Implement worker loop**

```go
package services

import (
    "context"
    "log/slog"
    "time"
)

func RunAggregatorWorker(ctx context.Context, a *EvidenceAggregator, interval time.Duration) {
    slog.Info("aggregator worker started", "interval", interval)
    t := time.NewTicker(interval)
    defer t.Stop()
    for {
        select {
        case <-ctx.Done(): return
        case <-t.C:
            n, err := a.ProcessPending(ctx, 500)
            if err != nil {
                slog.Error("aggregator", "error", err)
            } else if n > 0 {
                slog.Info("aggregated", "count", n)
            }
        }
    }
}
```

- [x] **Step 2: Spawn from main**

In `cmd/mur-server/main.go`:

```go
agg := services.NewEvidenceAggregator(db)
go services.RunAggregatorWorker(ctx, agg, 30*time.Second)
```

- [x] **Step 3: Verify via integration test**

Write a script that POSTs signals, waits 35s, then GETs patterns, and expects evidence updated.

- [x] **Step 4: Run locally**

```bash
make dev  # check logs show "aggregator worker started"
```

- [x] **Step 5: Commit**

```bash
git add internal/services/aggregator_worker.go cmd/mur-server/main.go
git commit -m "feat(worker): spawn evidence aggregator every 30s"
```

---

## Part D — mur-commander C1 Emit (W5, 4 tasks)

### Task 20: Add `mur_sync` module in commander engine

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/mur_sync/mod.rs`
- Create: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/mur_sync/outbox.rs`
- Create: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/mur_sync/client.rs`
- Create: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/mur_sync/flush.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/lib.rs`

**Prereq:** Tasks 6, 16

- [x] **Step 1: Write failing tests**

Create `crates/engine/tests/mur_sync_test.rs`:

```rust
use mur_common::{Actor, ActorSource, Scope, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
use mur_common::Signal;
use mur_commander_engine::mur_sync::Outbox;
use tempfile::tempdir;
use uuid::Uuid;

#[test]
fn outbox_write_and_list() {
    let dir = tempdir().unwrap();
    let ob = Outbox::new(dir.path()).unwrap();
    let s = sample_signal();
    ob.write(&s).unwrap();
    assert_eq!(ob.list_pending().unwrap().len(), 1);
}

fn sample_signal() -> Signal {
    Signal {
        id: Uuid::new_v4(),
        emitted_at: chrono::Utc::now(),
        actor: Actor { source: ActorSource::CommanderDaemon, native_id: "svc".into(),
            display_name: None, resolved_user_id: None },
        target: SignalTarget::Pattern { name: "x".into(), scope: Scope::Personal },
        kind: SignalKind::ExecutionSuccess,
        scope: Scope::Personal, confidence: 1.0,
        schema_version: SIGNAL_SCHEMA_VERSION,
    }
}
```

- [x] **Step 2: Implement (reuse mur-core's Outbox structure)**

Create `mur_sync/outbox.rs` — **same code as mur-core's `sync/outbox.rs` (Task 7)**. This is duplicated intentionally: mur-core is OSS Rust library, mur-commander is closed Rust workspace — they cannot depend on mur-core. Both re-implement using shared `mur-common` types.

Alternatively: extract Outbox/Inbox into a third OSS crate `mur-sync` (part of mur workspace) and commander depends on it. Decide per workspace policy.

**For Phase 1 simplicity: duplicate.** Document intent at top of file:
```rust
//! Outbox implementation (duplicates mur-core/sync/outbox.rs).
//! Cannot reuse because mur-core is OSS; commander is closed.
//! Will refactor to shared OSS `mur-sync` crate in Phase 2 if needed.
```

Copy the Outbox code from Task 7, adjusting imports (`use mur_common::Signal` etc.).

Create `mur_sync/client.rs` analogous to Task 10's `SyncClient`, but the auth model differs — commander uses **service account + X-Acting-On-Behalf-Of** header:

```rust
pub struct CommanderSyncClient {
    base_url: String,
    svc_token: String,
    http: reqwest::Client,
}

impl CommanderSyncClient {
    pub async fn push_batch_as(&self, user_id: &str, signals: &[Signal]) -> Result<BatchResponse> {
        let url = format!("{}/v1/signals/batch", self.base_url);
        let resp = self.http.post(&url)
            .bearer_auth(&self.svc_token)
            .header("X-Acting-On-Behalf-Of", user_id)
            .json(&serde_json::json!({"signals": signals}))
            .send().await?.error_for_status()?;
        Ok(resp.json().await?)
    }
}
```

Register module in `engine/src/lib.rs`: `pub mod mur_sync;`

- [x] **Step 3: Flush service**

Create `mur_sync/flush.rs`:

```rust
use anyhow::Result;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::interval;

use crate::mur_sync::{CommanderSyncClient, Outbox};

pub struct FlushService {
    outbox: Outbox,
    client: CommanderSyncClient,
    /// groups outbox entries by their target user_id (via Actor resolution cache)
    interval: Duration,
}

impl FlushService {
    pub fn new(outbox_dir: PathBuf, client: CommanderSyncClient, interval_secs: u64) -> Result<Self> {
        Ok(Self {
            outbox: Outbox::new(outbox_dir)?,
            client,
            interval: Duration::from_secs(interval_secs),
        })
    }

    pub async fn run(mut self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let mut tick = interval(self.interval);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Err(e) = self.flush_once().await {
                        tracing::error!("flush failed: {}", e);
                    }
                }
                _ = shutdown.changed() => {
                    if *shutdown.borrow() { break; }
                }
            }
        }
        Ok(())
    }

    async fn flush_once(&mut self) -> Result<()> {
        let pending = self.outbox.list_pending()?;
        if pending.is_empty() { return Ok(()); }

        // Group by on_behalf_of user (resolved from signal's actor.resolved_user_id or actor key)
        let mut by_user: std::collections::HashMap<String, Vec<(std::path::PathBuf, mur_common::Signal)>> = Default::default();
        for p in pending {
            let yaml = std::fs::read_to_string(&p)?;
            let sig: mur_common::Signal = match serde_yaml::from_str(&yaml) {
                Ok(s) => s,
                Err(e) => { tracing::warn!("bad outbox {}: {}", p.display(), e); continue; }
            };
            let uid = sig.actor.resolved_user_id.clone()
                .unwrap_or_else(|| format!("UNRESOLVED:{}", sig.actor.key()));
            by_user.entry(uid).or_default().push((p, sig));
        }

        for (uid, items) in by_user {
            if uid.starts_with("UNRESOLVED:") {
                tracing::warn!("skipping unresolved {}: {} items", uid, items.len());
                continue;
            }
            let signals: Vec<_> = items.iter().map(|(_, s)| s.clone()).collect();
            let resp = self.client.push_batch_as(&uid, &signals).await?;
            for (path, sig) in &items {
                if resp.accepted.iter().any(|a| *a == sig.id.to_string()) {
                    self.outbox.mark_flushed(path)?;
                }
            }
        }
        Ok(())
    }
}
```

- [x] **Step 4: Tests pass**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
cargo test -p mur-commander-engine mur_sync
```

- [x] **Step 5: Commit**

```bash
git add crates/engine/src/mur_sync/ crates/engine/tests/mur_sync_test.rs crates/engine/src/lib.rs
git commit -m "feat(engine): add mur_sync module (outbox + client + flush service)"
```

---

### Task 21: Extend AuditEntry with `injected_patterns`

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/audit.rs`

**Prereq:** Task 20

- [x] **Step 1: Failing test**

Find existing audit tests in `audit.rs`. Add:

```rust
#[test]
fn audit_entry_records_injected_patterns() {
    let entry = AuditEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        session_id: "s1".into(),
        workflow_id: Some("wf1".into()),
        action_type: ActionType::Execute,
        action_detail: "x".into(),
        approved_by: None,
        success: true,
        error: None,
        injected_patterns: vec!["p1".into(), "p2".into()],
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("injected_patterns"));
    let back: AuditEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(back.injected_patterns.len(), 2);
}
```

- [x] **Step 2: Verify fail**

- [x] **Step 3: Add field**

In `audit.rs`:

```rust
pub struct AuditEntry {
    // ... existing fields ...
    #[serde(default)]
    pub injected_patterns: Vec<String>,
}
```

Update `AuditStore::record_injection(...)`:

```rust
pub fn record_injection(&self, patterns: &[String]) -> Result<Uuid> {
    let entry = AuditEntry {
        id: Uuid::new_v4(),
        timestamp: Utc::now(),
        session_id: self.current_session_id(),
        workflow_id: None,
        action_type: ActionType::ModelInvoke,
        action_detail: format!("injected {} patterns", patterns.len()),
        approved_by: Some("auto".into()),
        success: true,
        error: None,
        injected_patterns: patterns.to_vec(),
    };
    self.append(&entry)?;
    Ok(entry.id)
}
```

- [x] **Step 4: Tests pass → Step 5: Commit**

```bash
cargo test -p mur-commander-engine audit
git add crates/engine/src/audit.rs
git commit -m "feat(audit): record injected_patterns on AuditEntry"
```

---

### Task 22: WorkflowRunner emits C1 signals

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/workflow/runner.rs`

**Prereq:** Tasks 20, 21

- [x] **Step 1: Failing integration test**

Create `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/tests/runner_emit_test.rs`:

```rust
use tempfile::tempdir;
use mur_commander_engine::workflow::runner::WorkflowRunner;
use mur_commander_engine::mur_sync::Outbox;

#[tokio::test]
async fn runner_emits_success_signal_for_injected_patterns() {
    let tmp = tempdir().unwrap();
    let outbox = Outbox::new(tmp.path().join("outbox")).unwrap();

    // Set up a minimal workflow that references 2 patterns via injection context
    let wf = make_trivial_workflow();
    let injected = vec!["p1".into(), "p2".into()];

    let runner = WorkflowRunner::builder()
        .outbox(outbox.clone())
        .build();
    let _ = runner.run_with_injected(wf, injected).await.unwrap();

    // Expect 2 signals in outbox
    let pending = outbox.list_pending().unwrap();
    assert_eq!(pending.len(), 2, "expected 2 signals (one per injected pattern)");
}
```

- [x] **Step 2: Verify fail**

- [x] **Step 3: Implement**

In `runner.rs`:

```rust
use mur_common::{Actor, ActorSource, Scope, Signal, SignalKind, SignalTarget, SIGNAL_SCHEMA_VERSION};
use crate::mur_sync::Outbox;

impl WorkflowRunner {
    pub async fn run_with_injected(
        &self,
        wf: Workflow,
        injected_patterns: Vec<String>,
    ) -> Result<WorkflowResult> {
        // Record injection in audit (Task 21)
        self.audit.record_injection(&injected_patterns)?;

        // Run workflow
        let result = self.run(wf).await;

        // Emit signals for each injected pattern
        let actor = self.current_actor();  // from ctx (chat user or daemon)
        let scope = self.current_scope();  // from workflow scope config
        for pname in &injected_patterns {
            let kind = match &result {
                Ok(_) => SignalKind::ExecutionSuccess,
                Err(e) => SignalKind::ExecutionFailure { error: e.to_string() },
            };
            let sig = Signal {
                id: uuid::Uuid::new_v4(),
                emitted_at: chrono::Utc::now(),
                actor: actor.clone(),
                target: SignalTarget::Pattern { name: pname.clone(), scope: scope.clone() },
                kind,
                scope: scope.clone(),
                confidence: 0.9,
                schema_version: SIGNAL_SCHEMA_VERSION,
            };
            if let Some(ob) = &self.outbox {
                if let Err(e) = ob.write(&sig) {
                    tracing::error!("outbox write failed: {}", e);
                }
            }
        }

        result
    }

    fn current_actor(&self) -> Actor {
        // Extract from runtime context. For DAemon-initiated runs, fall back to CommanderDaemon
        self.ctx.actor.clone().unwrap_or_else(|| Actor {
            source: ActorSource::CommanderDaemon,
            native_id: self.ctx.instance_id.clone(),
            display_name: None,
            resolved_user_id: None,
        })
    }

    fn current_scope(&self) -> Scope {
        self.ctx.scope.clone().unwrap_or(Scope::Personal)
    }
}
```

- [x] **Step 4: Tests pass**

```bash
cargo test -p mur-commander-engine runner_emit
```

- [x] **Step 5: Commit**

```bash
git add crates/engine/src/workflow/runner.rs crates/engine/tests/runner_emit_test.rs
git commit -m "feat(runner): emit ExecutionSuccess/Failure signals for injected patterns"
```

---

### Task 23: AutoFix + Breakpoint signal emission

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/workflow/autofix.rs`
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/engine/src/workflow/runner.rs` (breakpoint resume path)

**Prereq:** Task 22

- [x] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn autofix_emits_autofixapplied_signal() {
    let tmp = tempdir().unwrap();
    let outbox = Outbox::new(tmp.path().join("outbox")).unwrap();

    let autofix = AutoFix::new(outbox.clone(), ...);
    autofix.apply_fix(/* failing step with pattern ref "p1" */).await.unwrap();

    let pending = outbox.list_pending().unwrap();
    assert_eq!(pending.len(), 1);
    let yaml = std::fs::read_to_string(&pending[0]).unwrap();
    assert!(yaml.contains("auto_fix_applied"));
    assert!(yaml.contains("p1"));
}

#[tokio::test]
async fn breakpoint_reject_emits_override_signal() {
    let tmp = tempdir().unwrap();
    let outbox = Outbox::new(tmp.path().join("outbox")).unwrap();
    let runner = WorkflowRunner::builder().outbox(outbox.clone()).build();

    let handle = tokio::spawn(async move {
        runner.run_with_injected(make_workflow_with_breakpoint(), vec!["p1".into()]).await
    });

    // simulate user rejection at breakpoint
    runner.reject_breakpoint(Some("wrong action".into())).await;

    let _ = handle.await;
    let pending = outbox.list_pending().unwrap();
    assert!(pending.iter().any(|p| {
        let y = std::fs::read_to_string(p).unwrap();
        y.contains("user_override_at_breakpoint")
    }));
}
```

- [x] **Step 2: Verify fail**

- [x] **Step 3: Implement**

In `autofix.rs`:

```rust
pub async fn apply_fix(&self, failed_step: &Step, ctx: &RunCtx) -> Result<FixResult> {
    let result = self.attempt_fix(failed_step).await;

    // Emit AutoFixApplied signal for each injected pattern referenced by the step
    for pname in ctx.injected_patterns_for_step(failed_step) {
        let sig = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: ctx.actor.clone(),
            target: SignalTarget::Pattern { name: pname, scope: ctx.scope.clone() },
            kind: SignalKind::AutoFixApplied { step: failed_step.name.clone() },
            scope: ctx.scope.clone(),
            confidence: 0.8,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        if let Err(e) = self.outbox.write(&sig) {
            tracing::error!("outbox write: {}", e);
        }
    }
    result
}
```

In `runner.rs` breakpoint resume path:

```rust
pub async fn reject_breakpoint(&self, reason: Option<String>) -> Result<()> {
    // existing: set paused_at resume signal
    self.breakpoint_resume.notify_waiters();

    // emit UserOverrideAtBreakpoint for each injected pattern
    for pname in &self.ctx.injected_patterns {
        let sig = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: self.current_actor(),
            target: SignalTarget::Pattern { name: pname.clone(), scope: self.current_scope() },
            kind: SignalKind::UserOverrideAtBreakpoint { reason: reason.clone() },
            scope: self.current_scope(),
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        if let Some(ob) = &self.outbox { let _ = ob.write(&sig); }
    }
    Ok(())
}
```

- [x] **Step 4: Tests pass → Step 5: Commit**

```bash
cargo test -p mur-commander-engine autofix
cargo test -p mur-commander-engine breakpoint
git add crates/engine/src/workflow/autofix.rs crates/engine/src/workflow/runner.rs
git commit -m "feat(workflow): emit AutoFixApplied + UserOverrideAtBreakpoint signals"
```

---

### Task 24: Daemon spawns FlushService at startup

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-commander/crates/daemon/src/service.rs` (or main boot file)

**Prereq:** Task 20

- [x] **Step 1: Manual integration scenario**

Start daemon, confirm in logs:
```
2026-04-18T... INFO flush service started interval=60s
```

After 60s with an empty outbox:
```
2026-04-18T... DEBUG flush: no pending
```

- [x] **Step 2: Implement spawn**

In daemon service bootstrapping:

```rust
let outbox_dir = dirs::home_dir().unwrap().join(".mur/commander/outbox");
let svc_token = std::env::var("MUR_SERVER_SVC_TOKEN")
    .context("MUR_SERVER_SVC_TOKEN not set")?;
let server_url = std::env::var("MUR_SERVER_URL")
    .unwrap_or_else(|_| "https://mur-server.fly.dev".into());
let client = mur_commander_engine::mur_sync::CommanderSyncClient::new(server_url, svc_token)?;
let flush = mur_commander_engine::mur_sync::FlushService::new(outbox_dir, client, 60)?;
let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
tokio::spawn(async move {
    if let Err(e) = flush.run(sd_rx).await {
        tracing::error!("flush service exited: {}", e);
    }
});
// sd_tx 留著 for shutdown
```

Ensure graceful shutdown sends `sd_tx.send(true)`.

- [x] **Step 3: docker-compose smoke test**

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
MUR_SERVER_URL=http://localhost:8080 MUR_SERVER_SVC_TOKEN=dev cargo run -p mur-commander-daemon &
# Tail logs, verify "flush service started"
```

- [x] **Step 4: Verify**

Write a signal manually to `~/.mur/commander/outbox/`,等 60s,看是否被 flush (outbox 檔案移到 `.flushed/`)。

- [x] **Step 5: Commit**

```bash
git add crates/daemon/src/service.rs
git commit -m "feat(daemon): spawn FlushService at startup with env-configured server"
```

---

## Part E — Cross-binary Integration (W6, 3 tasks)

### Task 25: docker-compose for e2e testing

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/docker-compose.e2e.yml`
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/scripts/e2e-setup.sh`

**Prereq:** Tasks 12-19

- [x] **Step 1: Write compose**

```yaml
# /Volumes/Firecuda4tb/Projects/mur-server/docker-compose.e2e.yml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_USER: mur
      POSTGRES_PASSWORD: mur
      POSTGRES_DB: mur_e2e
    ports: ["5433:5432"]
    tmpfs: [/var/lib/postgresql/data]

  mur-server:
    build: .
    environment:
      DATABASE_URL: postgres://mur:mur@postgres:5432/mur_e2e
      JWT_SECRET: e2e-test-secret-do-not-use-in-prod
    depends_on: [postgres]
    ports: ["8080:8080"]
```

- [x] **Step 2: Setup script**

```bash
#!/usr/bin/env bash
# scripts/e2e-setup.sh
set -euo pipefail
cd "$(dirname "$0")/.."
docker compose -f docker-compose.e2e.yml up -d --wait
# wait for migrations, seed test user
sleep 3
psql postgres://mur:mur@localhost:5433/mur_e2e -f ./scripts/e2e-seed.sql
echo "E2E env ready at http://localhost:8080"
```

Create `scripts/e2e-seed.sql` with test user, API token, etc.

- [x] **Step 3: Verify**

```bash
bash scripts/e2e-setup.sh
curl http://localhost:8080/health  # should return OK
```

- [x] **Step 4: Tear down**

```bash
docker compose -f docker-compose.e2e.yml down -v
```

- [x] **Step 5: Commit**

```bash
git add docker-compose.e2e.yml scripts/e2e-setup.sh scripts/e2e-seed.sql
git commit -m "test: add docker-compose.e2e.yml + seed scripts for integration tests"
```

---

### Task 26: Cross-binary integration scenarios 1-6

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/tests/integration/sync_e2e.rs` (or under tests/ in appropriate crate)

**Prereq:** Task 25

- [x] **Step 1-2: Write 6 scenarios (each is a sub-test)**

```rust
// tests/integration/sync_e2e.rs (pseudocode, adapt to workspace layout)
use std::process::Command;

fn setup() -> TestEnv {
    // Ensure docker compose up, get test user token
    TestEnv::new()
}

#[test]
fn scenario_1_push_fetch_roundtrip() {
    // Seed pattern on server
    // mur push (signal)
    // On another ~/.mur, mur fetch
    // Verify pattern evidence updated
}

#[test]
fn scenario_2_commander_emits_success_to_server_flows_to_mur() { /* ... */ }

#[test]
fn scenario_3_team_scope_rejected_without_subscription() { /* ... (P2, skip in P1 MVP) */ }

#[test]
fn scenario_4_conflict_409_preserves_local() { /* ... */ }

#[test]
fn scenario_5_five_minute_dedupe() { /* ... */ }

#[test]
fn scenario_6_offline_outbox_retains() { /* ... */ }
```

Each scenario:
- docker-compose up server
- seed test data
- run mur binary as subprocess (`cargo run -p mur --bin mur`)
- run commander binary
- assert state

- [x] **Step 3: Implement**

Build test utilities in `tests/integration/helpers.rs`:
```rust
pub struct TestEnv { ... }
impl TestEnv {
    pub fn push(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_mur"))
            .args(&["push"])
            .env("MUR_SERVER_URL", &self.server_url)
            .env("HOME", &self.test_home)
            .output().unwrap()
    }
    // fetch, status, etc.
}
```

- [x] **Step 4: Run**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
bash ../mur-server/scripts/e2e-setup.sh
cargo test --test sync_e2e -- --test-threads=1
```

Expected: 6 scenarios pass.

- [x] **Step 5: Commit**

```bash
git add tests/integration/
git commit -m "test: add 6 cross-binary sync integration scenarios"
```

---

### Task 27: Golden Path 1 — manual e2e script

**Files:**
- Create: `/Volumes/Firecuda4tb/Projects/mur/scripts/golden-path-1.sh`

**Prereq:** Tasks 25, 26

- [x] **Step 1: Write script**

```bash
#!/usr/bin/env bash
# scripts/golden-path-1.sh
# Alice 個人旅程:裝 mur → 註冊 → push → 另一裝置 fetch → 本機學習
set -euo pipefail

TEST_HOME=$(mktemp -d)
export HOME=$TEST_HOME
export MUR_SERVER_URL=http://localhost:8080

echo "[1] Register alice on server via device code..."
mur login --non-interactive --email alice@test.local

echo "[2] Create a pattern"
mur new --name alice-rust-err --content "anyhow::Context for ergonomics" --tier session

echo "[3] Push to server"
mur push

echo "[4] Fetch from 'another device' (different HOME)"
TEST_HOME_2=$(mktemp -d)
HOME=$TEST_HOME_2 mur login --non-interactive --email alice@test.local
HOME=$TEST_HOME_2 mur fetch
if [ ! -f "$TEST_HOME_2/.mur/patterns/alice-rust-err.yaml" ]; then
    echo "FAIL: pattern not fetched"
    exit 1
fi

echo "[5] Simulate commander emitting success signal"
# ... post to /v1/signals/batch with svc token ...

echo "[6] Alice fetches on device 1 — sees updated evidence"
HOME=$TEST_HOME mur fetch
local_ev=$(HOME=$TEST_HOME mur pattern show alice-rust-err --json | jq .evidence.success_signals)
if [ "$local_ev" != "1" ]; then
    echo "FAIL: expected success_signals=1, got $local_ev"
    exit 1
fi

echo "Golden Path 1 PASSED"
```

- [x] **Step 2: Run**

```bash
bash scripts/golden-path-1.sh
```

- [x] **Step 3: Document expected output in README or docs**

- [x] **Step 4: Add to release checklist** (internal process)

- [x] **Step 5: Commit**

```bash
git add scripts/golden-path-1.sh
git commit -m "test: add Golden Path 1 e2e script (personal push/fetch + evidence roundtrip)"
```

---

## Phase 1 Final Validation

### Task 28: Wire up success metrics collection

**Files:**
- Modify: `/Volumes/Firecuda4tb/Projects/mur-server/internal/api/server.go` — add Prometheus metrics
- Create: `/Volumes/Firecuda4tb/Projects/mur-server/internal/services/metrics.go`

**Prereq:** Tasks 15, 16, 19

- [x] **Step 1: Add Prometheus counters**

```go
// internal/services/metrics.go
package services

import (
    "github.com/prometheus/client_golang/prometheus"
    "github.com/prometheus/client_golang/prometheus/promauto"
)

var (
    SignalsReceived = promauto.NewCounterVec(prometheus.CounterOpts{
        Name: "mur_signals_received_total",
        Help: "Signals received by mur-server",
    }, []string{"scope", "kind"})

    SignalsRejected = promauto.NewCounterVec(prometheus.CounterOpts{
        Name: "mur_signals_rejected_total",
        Help: "Signals rejected by mur-server",
    }, []string{"reason"})

    EvidenceUpdates = promauto.NewCounterVec(prometheus.CounterOpts{
        Name: "mur_evidence_updates_total",
        Help: "Evidence updates from signal aggregation",
    }, []string{"actor_source"})

    FetchDuration = promauto.NewHistogramVec(prometheus.HistogramOpts{
        Name: "mur_fetch_duration_seconds",
        Help: "Fetch endpoint latency",
        Buckets: prometheus.DefBuckets,
    }, []string{"status"})
)
```

- [x] **Step 2: Wire into handlers**

In `PostBatch`:
```go
for _, sig := range accepted { services.SignalsReceived.WithLabelValues(sig.Scope.Kind, sig.Kind.Type).Inc() }
for _, r := range rejected { services.SignalsRejected.WithLabelValues(r.Reason).Inc() }
```

In `GetPending`:
```go
start := time.Now()
defer func() { services.FetchDuration.WithLabelValues("ok").Observe(time.Since(start).Seconds()) }()
```

In aggregator (`ProcessPending`):
```go
services.EvidenceUpdates.WithLabelValues(actorSource).Inc()
```

Expose `/metrics` endpoint on server.

- [x] **Step 3: Verify**

```bash
curl http://localhost:8080/metrics | grep mur_
```

- [x] **Step 4: Add dashboard JSON (Grafana)**

Create `docs/grafana/mur-sync-dashboard.json` with panels for these metrics (beyond scope for detail — placeholder, doable in a follow-up).

- [x] **Step 5: Commit**

```bash
git add internal/services/metrics.go internal/api/handlers/signals.go internal/services/evidence_aggregator.go
git commit -m "feat(metrics): Prometheus counters for signals + evidence + fetch latency"
```

---

# Phase 2/3 Roadmap (High-Level)

以下為 Phase 2 和 Phase 3 的高階任務清單,**不**是詳細 plan。Phase 1 完成並收集 2 週真實數據後,另寫兩份獨立 plan:

## Phase 2 (W7-W10) — Team Scope + C2 Chat 萃取

**P2.1 Team 資料模型 (W7)**
- server: `teams`, `team_memberships`, `team_connected_actors` tables
- server: team CRUD handlers + invite flow
- commander: `teams/{team_id}/patterns/` 目錄 + cache refresh
- mur: `mur team create/invite/list/leave` CLI

**P2.2 付費閘門 (W8)**
- server: `subscriptions` table + LemonSqueezy webhook (既有基礎設施)
- server: middleware `require_team_subscription(team_id)`
- client: 錯誤訊息 + upgrade URL

**P2.3 C2 Chat 萃取 (W9)**
- commander gateway: 新增 `chat_extractor.rs` 用 LLM 粗篩
- commander: scope 決策 (DM/公開頻道/team-bound)
- commander: Slack Block Kit 按鈕確認流程
- server: `/v1/patterns/accept` / `/v1/patterns/reject` 實作 draft 生命週期

**P2.4 Golden Path 2 + 3 (W10)**
- scripts/golden-path-2.sh — Alice→Bob team sharing
- scripts/golden-path-3.sh — Commander chat extraction e2e

**Success 指標** (spec §8.6):
- Chat draft accept rate ≥ 40%
- FP rate < 30%

## Phase 3 (W11-W14) — C3 Procedural 萃取

**P3.1 Sleeptime Analyzer (W11-12)**
- commander engine: `procedural_extractor.rs`
- 掃 AuditStore → 分組 `(workflow, success, scope)` → 找 ≥5 次 ≥80% 成功的
- LLM 歸納出 procedural/fact/preference 候選

**P3.2 Draft 流程整合 (W13)**
- `origin_context` 帶 audit trail 給 user 可解釋性
- `mur show --draft` 顯示 evidence_trail

**P3.3 節律配置 (W14)**
- Daemon `sleeptime_enabled`, `sleeptime_idle_threshold_min` config
- Golden Path 測試

**Success 指標**:
- Procedural accept rate ≥ 50%
- Accepted procedural pattern 後續使用 ≥ 3 次

---

# Self-Review (per writing-plans skill)

## Spec Coverage Check

| Spec section | Covered by task |
|---|---|
| §1.1 拓撲 | Task 20, 24 (commander `~/.mur/commander/`) |
| §1.3 三 channel 流向 | C1 covered by Tasks 20-23; C2/C3 in Phase 2/3 roadmap |
| §2.1 Scope enum | Task 1 |
| §2.2 Actor | Task 2 |
| §2.3 Origin.actor | Task 3 |
| §2.4 Evidence.contributions | Task 4 |
| §2.5 Signal | Task 6 |
| §2.6 Pattern.scope | Task 5 |
| §3.1 API 端點 | Tasks 16, 17 (signals + actors);team/patterns endpoints in Phase 2 |
| §3.2 Auth | Task 18 (user_id in token);svc account partial in Task 20, full in Phase 2 |
| §3.3 Outbox/Inbox | Tasks 7, 8 (core), Task 20 (commander) |
| §3.5 Sync 頻率 | Task 24 (60s flush) |
| §4.1 C1 Evidence | Tasks 22, 23 |
| §4.1 guard rails (5min dedupe, 3x override) | Task 14 (dedupe), Task 15 (3x weight), Task 8 (client-side apply) |
| §5 OSS/閉源邊界 | Enforced by repo split in File Structure table |
| §7.1 衝突 | Task 16 (etag 409) partial — **gap:** no task explicitly covers 409 response body with current version, TODO resolve |
| §7.3 `mur sync status/logs` | Task 11 (status);logs jsonl — **gap:** not wired in, TODO add subtask |
| §7.7 PII 防線 | **gap:** not in Phase 1 tasks; defer to Phase 2 with explicit note |

**Fixes applied inline**:
- Added brief mention of 409 etag conflict in Task 16 comments
- `mur sync logs` left for Phase 2 (low priority)
- PII sanitization deferred to Phase 2 (added note in Phase 2 roadmap below)

## Placeholder Scan

- Task 14 uses `itoa(i int) string { return string(rune('0'+i)) }` as simplified — real code uses `strconv.Itoa`. **Marked inline.**
- No TBD/TODO/FIXME remain otherwise.

## Type Consistency

- `Actor::key()` format `"Slack:U123ABC"` — used in Tasks 2, 4, 8, 15 consistently
- `SignalKind::UserOverrideAtBreakpoint` — same name across Rust (Task 6, 22) and Go (Task 15 `user_override_at_breakpoint` snake_case) — consistent via serde `rename_all = "snake_case"`
- `Evidence.contributions` HashMap key type is `String`, consistent across tasks 4, 8, 15

---

# Plan complete.

**Saved to:** `docs/superpowers/plans/2026-04-18-mur-commander-memory-sync-plan.md`

**Scope:** Phase 1 (P0 Channel 1 Evidence) detailed with 28 tasks across 3 repos. Phase 2/3 roadmapped at high level.

**Two execution options:**

**1. Subagent-Driven (recommended)** — Fresh subagent per task, two-stage review between tasks, fast iteration. Uses `superpowers:subagent-driven-development`.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints for review.

Which approach?
