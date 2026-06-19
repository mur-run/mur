# MUR Fleet — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a working single-host MUR fleet you can create and run one iteration of — a named squad of agents collaborating over a shared signed channel via the existing DAG executor.

**Architecture:** A `Fleet` is one YAML file (`~/.mur/fleets/<name>/fleet.yaml`) + a struct. `mur fleet run` builds a `Procedure` with one delegate-step per member and calls the existing `execute_dag` with the fleet's channel id; members sign their replies into the channel (peer-writes-own). A `scope` field is added to `SkillManifest` so rules/skills can be tagged user/project/fleet/enterprise. No new runtime, transport, or event types.

**Tech Stack:** Rust (edition 2024), serde + serde_yaml, anyhow, tokio, clap (derive). Reuses `mur-core/src/executor/dag.rs`, `mur-channel` `ChannelService`, `mur-common` skill/channel types.

**Spec:** `docs/superpowers/specs/2026-06-19-mur-fleet-design.md`. **Branch:** `feat/mur-fleet`.

## Global Constraints

- Rust edition 2024 (`let` chains allowed). Build per-crate: `cargo build -p <crate>`.
- **Tests:** run targeted locally — `cargo test -p <crate> <test_name>` (CI uses `cargo nextest`; the full `cargo test --workspace` has known-flaky unrelated tests). Lint before commit: `cargo clippy -p <crate> -- -D warnings` and `cargo fmt`.
- **No hardcoded values** (mandatory rule #1): the concierge default router name lives in ONE constant `mur_common::fleet::CONCIERGE_AGENT` (value `"mur"`, the on-disk concierge dir).
- **Brand:** user-facing labels say "MUR" (uppercase); internal `name`/dir slugs stay lowercase. Fleet `name` is a lowercase slug == its directory.
- **Back-compat:** every new struct field is `#[serde(default)]` so existing `skill.yaml` files parse unchanged; new enum fields use `skip_serializing_if` to avoid rewriting existing files.
- **Atomic YAML writes:** temp file + rename (mirrors `mur-core/src/store/yaml.rs`).
- **File size ≤ 800 lines**; the fleet command is split per sub-command under `mur-core/src/cmd/fleet/`.
- Commit after every task. Conventional-commit messages, English.

## File Structure

**Create:**
- `mur-common/src/fleet.rs` — `Fleet` + `FleetLoop` types + `CONCIERGE_AGENT` const (types only, no I/O).
- `mur-core/src/cmd/fleet/mod.rs` — module decls + async `dispatch(FleetAction)`.
- `mur-core/src/cmd/fleet/store.rs` — load/save/list fleet YAML (I/O).
- `mur-core/src/cmd/fleet/create.rs` — `cmd_fleet_create`.
- `mur-core/src/cmd/fleet/list.rs` — `cmd_fleet_list`.
- `mur-core/src/cmd/fleet/show.rs` — `cmd_fleet_show`.
- `mur-core/src/cmd/fleet/run.rs` — `build_fleet_procedure` + `cmd_fleet_run`.

**Modify:**
- `mur-common/src/skill/manifest.rs` — add `SkillScope` enum + `scope`/`fleet`/`project` fields + `scope_visible`.
- `mur-common/src/lib.rs` — `pub mod fleet;`.
- `mur-channel/src/service.rs` — add `ChannelService::create_for_fleet`.
- `mur-core/src/cmd.rs` (or `cmd/mod.rs`) — `pub mod fleet;`.
- `mur-core/src/cli/actions.rs` — add `FleetAction` enum.
- `mur-core/src/cli/mod.rs` — add `Commands::Fleet { action }`.
- `mur-core/src/dispatch.rs` — add `Commands::Fleet` match arm.

---

### Task 1: SkillScope enum + SkillManifest fields

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` (struct at lines 33–88; insert fields after the `category` field)

**Interfaces:**
- Produces: `pub enum SkillScope { User, Project, Fleet, Enterprise }` (Default = User); `SkillScope::is_user(&self) -> bool`; new `SkillManifest` fields `scope: SkillScope`, `fleet: Option<String>`, `project: Option<String>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `mur-common/src/skill/manifest.rs`:

```rust
#[test]
fn skill_scope_serde_and_default() {
    assert_eq!(SkillScope::default(), SkillScope::User);
    assert_eq!(serde_yaml::to_string(&SkillScope::Fleet).unwrap().trim(), "fleet");
    let s: SkillScope = serde_yaml::from_str("project").unwrap();
    assert_eq!(s, SkillScope::Project);
    assert!(SkillScope::User.is_user());
    assert!(!SkillScope::Fleet.is_user());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common skill_scope_serde_and_default`
Expected: FAIL — `cannot find type SkillScope`.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `mur-common/src/skill/manifest.rs` (after imports):

```rust
/// Visibility scope for a skill/rule. Higher scope wins on conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum SkillScope {
    #[default]
    User,
    Project,
    Fleet,
    Enterprise,
}

impl SkillScope {
    /// Used as `skip_serializing_if` so existing skill.yaml files don't gain a `scope:` line.
    pub fn is_user(&self) -> bool {
        matches!(self, SkillScope::User)
    }
}
```

Insert these fields into the `SkillManifest` struct, immediately after the `category` field:

```rust
    #[serde(default, skip_serializing_if = "SkillScope::is_user")]
    pub scope: SkillScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common skill_scope_serde_and_default`
Expected: PASS. Then `cargo build -p mur-common` (confirms every other construction of `SkillManifest` still compiles because the new fields default).

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-common -- -D warnings
git add mur-common/src/skill/manifest.rs
git commit -m "feat(fleet): add SkillScope + scope/fleet/project fields to SkillManifest"
```

---

### Task 2: scope_visible predicate

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` (add free function + tests)

**Interfaces:**
- Produces: `pub fn scope_visible(scope: SkillScope, skill_fleet: Option<&str>, skill_project: Option<&str>, active_fleet: Option<&str>, active_project: Option<&str>) -> bool`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn scope_visible_matrix() {
    // user + enterprise always visible
    assert!(scope_visible(SkillScope::User, None, None, None, None));
    assert!(scope_visible(SkillScope::Enterprise, None, None, None, None));
    // fleet skill visible only when active fleet matches
    assert!(scope_visible(SkillScope::Fleet, Some("dev"), None, Some("dev"), None));
    assert!(!scope_visible(SkillScope::Fleet, Some("dev"), None, Some("ops"), None));
    assert!(!scope_visible(SkillScope::Fleet, Some("dev"), None, None, None));
    // project skill visible only when active project matches
    assert!(scope_visible(SkillScope::Project, None, Some("/p"), None, Some("/p")));
    assert!(!scope_visible(SkillScope::Project, None, Some("/p"), None, Some("/q")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common scope_visible_matrix`
Expected: FAIL — `cannot find function scope_visible`.

- [ ] **Step 3: Write minimal implementation**

Add to `mur-common/src/skill/manifest.rs` (after the `impl SkillScope` block):

```rust
/// Is a skill with this (scope, fleet, project) visible in the given active context?
/// Layers combine: user/enterprise are always visible; fleet/project are visible
/// only when their selector matches the active context. (specific wins; see spec §6)
pub fn scope_visible(
    scope: SkillScope,
    skill_fleet: Option<&str>,
    skill_project: Option<&str>,
    active_fleet: Option<&str>,
    active_project: Option<&str>,
) -> bool {
    match scope {
        SkillScope::User | SkillScope::Enterprise => true,
        SkillScope::Fleet => matches!((skill_fleet, active_fleet), (Some(f), Some(a)) if f == a),
        SkillScope::Project => matches!((skill_project, active_project), (Some(p), Some(a)) if p == a),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common scope_visible_matrix`
Expected: PASS.

> **Phase-1 note (deliberate ponytail deferral):** wiring `scope_visible` into the live injection
> path (`mur-core/src/inject/hook.rs`) is **Phase 2**, because the active-fleet context only exists
> once the fleet loop sets it. Phase 1 ships the data model + predicate (immediately usable, fully
> tested). `// ponytail: predicate ready; wire into inject/hook.rs when fleet runtime context lands (Phase 2)`.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-common -- -D warnings
git add mur-common/src/skill/manifest.rs
git commit -m "feat(fleet): scope_visible precedence predicate for scoped skills"
```

---

### Task 3: Fleet type in mur-common

**Files:**
- Create: `mur-common/src/fleet.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod fleet;` alphabetically among the existing `pub mod` lines)

**Interfaces:**
- Produces: `Fleet { name, display_name, goal, router: Option<String>, members: Vec<String>, channel_id, rules, skills, loop_cfg: Option<FleetLoop> }`; `FleetLoop { trigger, max_iterations, budget_usd, deadline, done_when }`; `Fleet::router_or_concierge(&self) -> &str`; `pub const CONCIERGE_AGENT: &str = "mur"`.

- [ ] **Step 1: Write the failing test**

Create `mur-common/src/fleet.rs` with only this test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_yaml_roundtrip_and_router_default() {
        let f = Fleet {
            name: "dev".into(),
            display_name: "Dev Team".into(),
            goal: "ship it".into(),
            router: None,
            members: vec!["pm".into(), "qa".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
        };
        assert_eq!(f.router_or_concierge(), CONCIERGE_AGENT);
        let yaml = serde_yaml::to_string(&f).unwrap();
        let back: Fleet = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, f);
        // `loop:` key (not `loop_cfg`) when present
        let with_loop: Fleet = serde_yaml::from_str(
            "name: dev\nchannel_id: fleet-dev\nloop:\n  trigger: manual\n  max_iterations: 3\n  budget_usd: 1.0\n",
        ).unwrap();
        assert_eq!(with_loop.loop_cfg.unwrap().max_iterations, 3);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common fleet_yaml_roundtrip_and_router_default`
Expected: FAIL — `cannot find type Fleet` / module `fleet` not declared.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-common/src/fleet.rs` (above the test module):

```rust
//! Fleet — a named squad of agents working a shared goal over one channel.
//! Types only; all I/O lives in `mur-core` (per the mur-common "types only" rule).

use serde::{Deserialize, Serialize};

/// On-disk concierge/default-router agent name (the `~/.mur/agents/mur` dir).
pub const CONCIERGE_AGENT: &str = "mur";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    #[serde(default)]
    pub members: Vec<String>,
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_cfg: Option<FleetLoop>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FleetLoop {
    #[serde(default = "default_trigger")]
    pub trigger: String, // "manual" | "cron:<expr>"
    pub max_iterations: u32,
    pub budget_usd: f64,
    #[serde(default)]
    pub deadline: String, // humantime, e.g. "2h"
    #[serde(default)]
    pub done_when: String,
}

fn default_trigger() -> String {
    "manual".to_string()
}

impl Fleet {
    /// The router agent, falling back to the concierge.
    pub fn router_or_concierge(&self) -> &str {
        self.router.as_deref().unwrap_or(CONCIERGE_AGENT)
    }
}
```

Add to `mur-common/src/lib.rs`:

```rust
pub mod fleet;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common fleet_yaml_roundtrip_and_router_default`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-common -- -D warnings
git add mur-common/src/fleet.rs mur-common/src/lib.rs
git commit -m "feat(fleet): Fleet + FleetLoop types in mur-common"
```

---

### Task 4: Fleet store (load/save/list) + module wiring

**Files:**
- Create: `mur-core/src/cmd/fleet/mod.rs`, `mur-core/src/cmd/fleet/store.rs`
- Modify: `mur-core/src/cmd.rs` (or `cmd/mod.rs`) — add `pub mod fleet;`

**Interfaces:**
- Consumes: `mur_common::fleet::Fleet`.
- Produces: `fleet::store::{fleets_dir, fleet_dir, fleet_path, save_fleet, load_fleet, list_fleets}`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/store.rs` with the impl + this test:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;

    #[test]
    fn save_load_list_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let f = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(),
            router: None, members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        save_fleet(home, &f).unwrap();
        assert!(fleet_path(home, "dev").exists());
        assert_eq!(load_fleet(home, "dev").unwrap(), f);
        assert_eq!(list_fleets(home).unwrap(), vec!["dev".to_string()]);
        assert!(load_fleet(home, "missing").is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet::store::tests::save_load_list_roundtrip`
Expected: FAIL — module `fleet` not declared / functions missing.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/store.rs`:

```rust
//! Fleet persistence: `~/.mur/fleets/<name>/fleet.yaml` (atomic writes).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mur_common::fleet::Fleet;

pub fn fleets_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("fleets")
}
pub fn fleet_dir(mur_home: &Path, name: &str) -> PathBuf {
    fleets_dir(mur_home).join(name)
}
pub fn fleet_path(mur_home: &Path, name: &str) -> PathBuf {
    fleet_dir(mur_home, name).join("fleet.yaml")
}

pub fn save_fleet(mur_home: &Path, fleet: &Fleet) -> Result<()> {
    let dir = fleet_dir(mur_home, &fleet.name);
    std::fs::create_dir_all(&dir)?;
    let path = fleet_path(mur_home, &fleet.name);
    let yaml = serde_yaml::to_string(fleet)?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml)?;
    std::fs::rename(&tmp, &path)?; // atomic
    Ok(())
}

pub fn load_fleet(mur_home: &Path, name: &str) -> Result<Fleet> {
    let path = fleet_path(mur_home, name);
    let yaml = std::fs::read_to_string(&path)
        .with_context(|| format!("fleet '{name}' not found at {}", path.display()))?;
    Ok(serde_yaml::from_str(&yaml)?)
}

pub fn list_fleets(mur_home: &Path) -> Result<Vec<String>> {
    let dir = fleets_dir(mur_home);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut names = vec![];
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        if entry.path().join("fleet.yaml").exists() {
            if let Some(n) = entry.file_name().to_str() {
                names.push(n.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}
```

Create `mur-core/src/cmd/fleet/mod.rs`:

```rust
//! `mur fleet` — squads of agents working a shared goal. (Phase 1: manual run.)

pub mod store;
```

Add to `mur-core/src/cmd.rs` (or `cmd/mod.rs`), among the other `pub mod` lines:

```rust
pub mod fleet;
```

Confirm `tempfile` is a dev-dependency of `mur-core` (it is used by other tests). If not, add it under `[dev-dependencies]` in `mur-core/Cargo.toml`: `tempfile = "3"`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet::store::tests::save_load_list_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/ mur-core/src/cmd.rs
git commit -m "feat(fleet): fleet YAML store (load/save/list) + cmd module"
```

---

### Task 5: ChannelService::create_for_fleet

**Files:**
- Modify: `mur-channel/src/service.rs` (add method to `impl ChannelService`, mirroring `create_for_agent` at lines 70–99)

**Interfaces:**
- Consumes: existing `Channel`, `Participant`, `ParticipantRole`, `ChannelActor`, `ChannelState`, `Goal`, `CHANNEL_SCHEMA_VERSION` (already imported in this file).
- Produces: `ChannelService::create_for_fleet(&self, fleet_name: &str, router: &str, members: &[String]) -> Result<Channel>`. Channel id == `fleet-<name>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `mur-channel/src/service.rs`:

```rust
#[test]
fn create_for_fleet_sets_roles_and_id() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_fleet("dev", "mur", &["pm".to_string(), "qa".to_string()])
        .unwrap();
    assert_eq!(ch.id, "fleet-dev");
    // owner human + router + 2 delegates = 4 participants
    assert_eq!(ch.participants.len(), 4);
    assert!(ch.participants.iter().any(|p| p.role == ParticipantRole::Router
        && matches!(&p.actor, ChannelActor::Agent { id } if id == "mur")));
    assert_eq!(
        ch.participants.iter().filter(|p| p.role == ParticipantRole::Delegate).count(),
        2
    );
    // persisted
    assert_eq!(svc.load_events(&ch.id).unwrap().len(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-channel create_for_fleet_sets_roles_and_id`
Expected: FAIL — `no method named create_for_fleet`.

- [ ] **Step 3: Write minimal implementation**

Add inside `impl ChannelService` (after `create_for_agent`):

```rust
/// Create the long-lived shared channel for a fleet. Id is the stable,
/// filesystem-safe `fleet-<name>`. Router gets `Router`, members `Delegate`.
pub fn create_for_fleet(
    &self,
    fleet_name: &str,
    router: &str,
    members: &[String],
) -> Result<Channel> {
    let now = Utc::now();
    let mut participants = vec![
        Participant {
            actor: ChannelActor::local_human(),
            role: ParticipantRole::Owner,
            joined_at: now,
        },
        Participant {
            actor: ChannelActor::Agent { id: router.to_string() },
            role: ParticipantRole::Router,
            joined_at: now,
        },
    ];
    for m in members {
        participants.push(Participant {
            actor: ChannelActor::Agent { id: m.clone() },
            role: ParticipantRole::Delegate,
            joined_at: now,
        });
    }
    let ch = Channel {
        v: CHANNEL_SCHEMA_VERSION,
        id: format!("fleet-{fleet_name}"),
        title: format!("fleet: {fleet_name}"),
        goal: Goal::default(),
        state: ChannelState::Working,
        owner: ChannelActor::local_human(),
        participants,
        created_at: now,
        updated_at: now,
    };
    self.store.create(&ch)?;
    self.index.upsert(&ch)?;
    Ok(ch)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-channel create_for_fleet_sets_roles_and_id`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-channel -- -D warnings
git add mur-channel/src/service.rs
git commit -m "feat(fleet): ChannelService::create_for_fleet (router+members participants)"
```

---

### Task 6: cmd_fleet_create

**Files:**
- Create: `mur-core/src/cmd/fleet/create.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `pub mod create;`)

**Interfaces:**
- Consumes: `fleet::store`, `mur_channel::ChannelService::create_for_fleet`, `mur_common::fleet::{Fleet, CONCIERGE_AGENT}`.
- Produces: `create::cmd_fleet_create(mur_home: &Path, name: &str, members: Vec<String>, router: Option<String>, goal: Option<String>) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/create.rs` with impl + test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_writes_fleet_and_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        cmd_fleet_create(home, "dev", vec!["pm".into()], None, Some("ship".into())).unwrap();
        let f = super::super::store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.channel_id, "fleet-dev");
        assert_eq!(f.goal, "ship");
        assert_eq!(f.router_or_concierge(), mur_common::fleet::CONCIERGE_AGENT);
        // second create errors (already exists)
        assert!(cmd_fleet_create(home, "dev", vec![], None, None).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet::create::tests::create_writes_fleet_and_channel`
Expected: FAIL — module `create` not declared / function missing.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/create.rs`:

```rust
//! `mur fleet create` — write fleet.yaml + create the shared channel.

use std::path::Path;

use anyhow::{bail, Result};
use mur_common::fleet::{Fleet, CONCIERGE_AGENT};

use super::store;

pub fn cmd_fleet_create(
    mur_home: &Path,
    name: &str,
    members: Vec<String>,
    router: Option<String>,
    goal: Option<String>,
) -> Result<()> {
    if store::fleet_path(mur_home, name).exists() {
        bail!("fleet '{name}' already exists");
    }
    let router_name = router.clone().unwrap_or_else(|| CONCIERGE_AGENT.to_string());

    let svc = mur_channel::ChannelService::open(mur_home)?;
    let ch = svc.create_for_fleet(name, &router_name, &members)?;

    let fleet = Fleet {
        name: name.to_string(),
        display_name: String::new(),
        goal: goal.unwrap_or_default(),
        router,
        members,
        channel_id: ch.id.clone(),
        rules: vec![],
        skills: vec![],
        loop_cfg: None,
    };
    store::save_fleet(mur_home, &fleet)?;
    println!("Created fleet '{name}' (channel {})", ch.id);
    Ok(())
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod create;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet::create::tests::create_writes_fleet_and_channel`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/
git commit -m "feat(fleet): mur fleet create (writes fleet.yaml + shared channel)"
```

---

### Task 7: cmd_fleet_list + cmd_fleet_show

**Files:**
- Create: `mur-core/src/cmd/fleet/list.rs`, `mur-core/src/cmd/fleet/show.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `pub mod list;` and `pub mod show;`)

**Interfaces:**
- Produces: `list::cmd_fleet_list(mur_home: &Path) -> Result<()>`; `show::cmd_fleet_show(mur_home: &Path, name: &str) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/show.rs` with impl + test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_errors_when_missing_ok_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(cmd_fleet_show(home, "dev").is_err());
        super::super::create::cmd_fleet_create(home, "dev", vec!["pm".into()], None, None).unwrap();
        assert!(cmd_fleet_show(home, "dev").is_ok());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet::show::tests::show_errors_when_missing_ok_when_present`
Expected: FAIL — module/function missing.

- [ ] **Step 3: Write minimal implementation**

Create `mur-core/src/cmd/fleet/show.rs`:

```rust
//! `mur fleet show` — roster + goal.

use std::path::Path;

use anyhow::Result;

use super::store;

pub fn cmd_fleet_show(mur_home: &Path, name: &str) -> Result<()> {
    let f = store::load_fleet(mur_home, name)?;
    println!("Fleet: {}", f.name);
    println!("Goal: {}", f.goal);
    println!("Router: {}", f.router_or_concierge());
    println!("Members: {}", f.members.join(", "));
    println!("Channel: {}", f.channel_id);
    if !f.rules.is_empty() {
        println!("Rules: {}", f.rules.join(", "));
    }
    if !f.skills.is_empty() {
        println!("Skills: {}", f.skills.join(", "));
    }
    Ok(())
}
```

Create `mur-core/src/cmd/fleet/list.rs`:

```rust
//! `mur fleet list` — all fleets.

use std::path::Path;

use anyhow::Result;

use super::store;

pub fn cmd_fleet_list(mur_home: &Path) -> Result<()> {
    let names = store::list_fleets(mur_home)?;
    if names.is_empty() {
        println!("No fleets. Create one: mur fleet create <name> --members a,b,c --goal \"...\"");
        return Ok(());
    }
    for n in names {
        let f = store::load_fleet(mur_home, &n)?;
        println!(
            "{}  members=[{}]  router={}  goal={}",
            f.name,
            f.members.join(","),
            f.router_or_concierge(),
            f.goal
        );
    }
    Ok(())
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod list;
pub mod show;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet::show::tests::show_errors_when_missing_ok_when_present`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/
git commit -m "feat(fleet): mur fleet list + show"
```

---

### Task 8: cmd_fleet_run (build Procedure + execute_dag + print)

**Files:**
- Create: `mur-core/src/cmd/fleet/run.rs`
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add `pub mod run;`)

**Interfaces:**
- Consumes: `mur_common::skill::manifest::{Procedure, ProcedureStep}`; `crate::executor::dag::{DagExecOptions, execute_dag}`; `mur_channel::ChannelService`; `mur_common::channel::ChannelActor`.
- Produces: `run::build_fleet_procedure(goal: &str, members: &[String]) -> Procedure`; `run::cmd_fleet_run(mur_home: &Path, name: &str) -> Result<()>` (async).

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/fleet/run.rs` with impl + test (the pure builder is the unit-tested logic; the dial path is validated live):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fleet_procedure_one_delegate_step_per_member() {
        let p = build_fleet_procedure("ship it", &["pm".to_string(), "qa".to_string()]);
        assert_eq!(p.steps.len(), 2);
        assert_eq!(p.steps[0].delegate_to.as_deref(), Some("pm"));
        assert_eq!(p.steps[1].delegate_to.as_deref(), Some("qa"));
        assert_eq!(p.steps[0].intent.as_deref(), Some("ship it"));
        assert!(p.steps[0].depends_on.is_empty()); // parallel rank 0
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core fleet::run::tests::build_fleet_procedure_one_delegate_step_per_member`
Expected: FAIL — module/function missing.

- [ ] **Step 3: Write minimal implementation**

Prepend to `mur-core/src/cmd/fleet/run.rs`:

```rust
//! `mur fleet run` — one iteration: fan the goal out to each member over the
//! shared channel via the existing DAG executor (delegation), then print replies.

use std::path::Path;

use anyhow::{bail, Result};
use mur_common::channel::ChannelActor;
use mur_common::skill::manifest::{Procedure, ProcedureStep};

use super::store;

/// Phase 1 "plan": one parallel delegate-step per member, each handed the goal.
/// (Phase 2 replaces this with a router-produced DAG.)
pub fn build_fleet_procedure(goal: &str, members: &[String]) -> Procedure {
    Procedure {
        variables: vec![],
        steps: members
            .iter()
            .map(|m| ProcedureStep {
                description: format!("{m}: {goal}"),
                intent: Some(goal.to_string()),
                delegate_to: Some(m.clone()),
                id: Some(m.clone()),
                ..Default::default()
            })
            .collect(),
    }
}

pub async fn cmd_fleet_run(mur_home: &Path, name: &str) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;
    if fleet.members.is_empty() {
        bail!("fleet '{name}' has no members");
    }
    let proc = build_fleet_procedure(&fleet.goal, &fleet.members);
    let opts = crate::executor::dag::DagExecOptions {
        yes: true,
        channel_id: Some(fleet.channel_id.clone()),
        run_id: format!("run-{}", uuid::Uuid::now_v7()),
        ..Default::default()
    };
    // skill_name here is just a label for the run; the fleet channel id is reused.
    let out = crate::executor::dag::execute_dag(mur_home, &fleet.channel_id, &proc, &opts).await?;
    if let Some(t) = out.output_text {
        if !t.is_empty() {
            println!("{t}");
        }
    }

    // Tail agent-authored replies written into the shared channel (peer-writes-own).
    // ponytail: prints payload["text"]; confirm the exact reply payload shape in the live Harness test.
    let svc = mur_channel::ChannelService::open(mur_home)?;
    for ev in svc.load_events(&fleet.channel_id)? {
        if let ChannelActor::Agent { id } = &ev.actor {
            if let Some(text) = ev.payload.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    println!("[{id}] {text}");
                }
            }
        }
    }
    Ok(())
}
```

Add to `mur-core/src/cmd/fleet/mod.rs`:

```rust
pub mod run;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core fleet::run::tests::build_fleet_procedure_one_delegate_step_per_member`
Expected: PASS. Then `cargo build -p mur-core`.

> **Live behavior** (members actually replying) is validated in the Harness E2E below, not a unit test — `execute_dag` dials running agent runtimes over A2A sockets.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy -p mur-core -- -D warnings
git add mur-core/src/cmd/fleet/
git commit -m "feat(fleet): mur fleet run (one iteration via DAG delegation + reply tail)"
```

---

### Task 9: CLI wiring (clap + dispatch)

**Files:**
- Modify: `mur-core/src/cli/actions.rs` (add `FleetAction`, near `TeamAction` at line 404)
- Modify: `mur-core/src/cli/mod.rs` (add `Commands::Fleet`, near `Team` at line 174; import `FleetAction`)
- Modify: `mur-core/src/cmd/fleet/mod.rs` (add async `dispatch`)
- Modify: `mur-core/src/dispatch.rs` (add `Commands::Fleet` arm near line 268; import `FleetAction` in the `use` at line 15)

**Interfaces:**
- Consumes: `create::cmd_fleet_create`, `list::cmd_fleet_list`, `show::cmd_fleet_show`, `run::cmd_fleet_run`.
- Produces: `cmd::fleet::dispatch(action: FleetAction) -> Result<()>`; `Commands::Fleet { action: FleetAction }`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `mur-core/src/cli/mod.rs` (mirror any existing clap parse test; the root parser type is the `#[derive(Parser)]` struct in this file — use its real name, shown as `Cli` below):

```rust
#[test]
fn cli_parses_fleet_create() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "mur", "fleet", "create", "dev", "--members", "pm,qa", "--goal", "ship",
    ])
    .unwrap();
    match cli.command {
        Commands::Fleet {
            action: crate::cli::actions::FleetAction::Create { name, members, goal, .. },
        } => {
            assert_eq!(name, "dev");
            assert_eq!(members, vec!["pm".to_string(), "qa".to_string()]);
            assert_eq!(goal.as_deref(), Some("ship"));
        }
        _ => panic!("expected fleet create"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core cli_parses_fleet_create`
Expected: FAIL — no `Commands::Fleet` / `FleetAction`.

- [ ] **Step 3: Write minimal implementation**

In `mur-core/src/cli/actions.rs` (top already has `use clap::Subcommand;`), add:

```rust
#[derive(Debug, Subcommand)]
pub enum FleetAction {
    /// Create a new fleet (squad of agents + a goal + a shared channel)
    Create {
        /// Fleet name (lowercase slug)
        name: String,
        /// Comma-separated member agent names
        #[arg(long, value_delimiter = ',')]
        members: Vec<String>,
        /// Router agent (defaults to the concierge `mur`)
        #[arg(long)]
        router: Option<String>,
        /// One-line goal
        #[arg(long)]
        goal: Option<String>,
    },
    /// List all fleets
    List,
    /// Show a fleet's roster + goal
    Show {
        /// Fleet name
        name: String,
    },
    /// Run one fleet iteration (Phase 1)
    Run {
        /// Fleet name
        name: String,
    },
}
```

In `mur-core/src/cli/mod.rs`, ensure `FleetAction` is in scope (extend the existing `use crate::cli::actions::{...}` or `use super::actions::{...}` import to include `FleetAction`), then add this variant to the `Commands` enum (next to `Team`):

```rust
    /// Manage fleets — squads of agents working a shared goal
    Fleet {
        #[command(subcommand)]
        action: FleetAction,
    },
```

In `mur-core/src/cmd/fleet/mod.rs`, append the dispatcher:

```rust
use anyhow::Result;

use crate::cli::actions::FleetAction;

pub async fn dispatch(action: FleetAction) -> Result<()> {
    let mur_home = crate::paths::mur_root(None);
    match action {
        FleetAction::Create { name, members, router, goal } => {
            create::cmd_fleet_create(&mur_home, &name, members, router, goal)
        }
        FleetAction::List => list::cmd_fleet_list(&mur_home),
        FleetAction::Show { name } => show::cmd_fleet_show(&mur_home, &name),
        FleetAction::Run { name } => run::cmd_fleet_run(&mur_home, &name).await,
    }
}
```

In `mur-core/src/dispatch.rs`, add `FleetAction` to the `use crate::cli::actions::{...}` list (line ~15), then add the arm to the top-level `match` (next to `Commands::Team`):

```rust
        Commands::Fleet { action } => cmd::fleet::dispatch(action).await?,
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core cli_parses_fleet_create`
Expected: PASS.
Then full build + lint: `cargo build -p mur-core && cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add mur-core/src/cli/ mur-core/src/dispatch.rs mur-core/src/cmd/fleet/mod.rs
git commit -m "feat(fleet): wire mur fleet {create,list,show,run} into the CLI"
```

---

### Task 10: Harness E2E + build/install

**Files:** none (verification task).

- [ ] **Step 1: Build the binary**

Run: `cargo build -p mur-core --release` (or `./build.sh` if the dashboard build is wanted).
Expected: builds clean.

- [ ] **Step 2: Create two real agents (if not present) and a fleet**

```bash
mur agent create pm  --display-name PM  || true
mur agent create qa  --display-name QA  || true
mur fleet create demo --members pm,qa --goal "Say hello and state your role."
mur fleet show demo
mur fleet list
```
Expected: `fleet show` prints router=mur, members pm,qa, channel `fleet-demo`; `~/.mur/fleets/demo/fleet.yaml` exists.

- [ ] **Step 3: Run one iteration and observe replies in the channel**

```bash
mur fleet run demo
mur channel show fleet-demo   # or: tail ~/.mur/channels/fleet-demo/events.jsonl
```
Expected: each member's reply appears, attributed to `Agent{pm}` / `Agent{qa}`, signed (peer-writes-own). If the reply tail prints nothing, inspect one event's payload shape in `events.jsonl` and adjust the `payload["text"]` extraction in `run.rs` (the ponytail-marked line), then re-run.

- [ ] **Step 4: Update docs**

Add `mur fleet {create|list|show|run}` to the CLI surface section of `CLAUDE.md` and to `README.md`. Commit:

```bash
git add CLAUDE.md README.md
git commit -m "docs(fleet): document mur fleet Phase 1 CLI"
```

- [ ] **Step 5: Full validation**

Run: `cargo nextest run -p mur-common -p mur-core -p mur-channel` (or targeted `cargo test -p ...` per crate).
Expected: fleet tests green. Then `cargo fmt --check` and per-crate clippy clean.

---

## Self-Review

**Spec coverage:**
- Fleet object + `fleet.yaml` → Tasks 3, 4. ✅
- CLI `mur fleet {create,list,show,run}` → Tasks 6, 7, 8, 9. ✅
- `run` does one iteration over `fleet-<name>` via `execute_dag` → Task 8. ✅
- `scope` field on `SkillManifest` (+ predicate; live wiring deferred to Phase 2 with rationale) → Tasks 1, 2. ✅
- Reuse-first (channels, dag.rs, channel_writer via dag, team_cmd clap shape) → Tasks 5, 8, 9. ✅
- Router default = concierge `mur` (single const, no hardcode) → Task 3 (`CONCIERGE_AGENT`). ✅
- Channel id filesystem-safe (`fleet-<name>`, no colon) → Task 5. ✅
- Out of scope by design (Phase 2+): loop/`fleet_tick`, guards, harvest scope-stamping, live scope injection, commander/server. Noted, not built.

**Placeholder scan:** every code step shows full code; every test shows real assertions; exact `cargo test` invocations with expected output. No TBD/TODO. The single deliberate runtime-shape uncertainty (reply payload key) is marked with a `// ponytail:` note and a Task-10 verification step, not left as a silent placeholder.

**Type consistency:** `SkillScope`, `scope_visible`, `Fleet`/`FleetLoop`/`loop_cfg`/`router_or_concierge`/`CONCIERGE_AGENT`, `create_for_fleet(fleet_name, router, members)`, `build_fleet_procedure(goal, members)`, `cmd_fleet_{create,list,show,run}`, `FleetAction`/`Commands::Fleet`, `crate::paths::mur_root(None)`, `crate::executor::dag::{DagExecOptions, execute_dag}` — names used in later tasks match their definitions in earlier tasks. ✅
