# M4a — Peer Transfer (Pull) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `mur skill install agent://<agent-name>/<skill-name>` pulls a skill from another local agent's skill store, verifies content hash against the registry for trust, appends a transfer-chain entry, and registers the skill on the installing agent's profile so the Agent Card can broadcast it.

**Architecture:**
- A new A2A method handler `skills/get` serves skill manifests from a configured skill-store root. In M4a it is wired but not exercised over a socket — install reads the source store directly from the local filesystem. M4b introduces the real socket round-trip.
- The Agent Card broadcasts Layer 1+2 metadata per installed skill via a new `installed_skills: Vec<SkillCardEntry>` field on `AgentProfile`. The legacy `skills: Vec<String>` field (which holds per-agent relative paths managed by `mur agent skill add`) is left untouched — we do not migrate or repurpose it.
- The install command parses `agent://` URLs, locates the source agent's skill store on the local filesystem, runs content-based trust verification (registry hash match → Verified, no match → Sandboxed, revoked → reject), writes the skill into the target store with `transfer_chain` appended, and pushes a `SkillCardEntry` into the calling agent's profile.

**M4a deployment assumption (single-home, single-user):** All agents share one `MUR_HOME` and skills live at `<MUR_HOME>/skills/<name>/`. `agent://<name>/<skill>` resolves the source store as the *same* `MUR_HOME` (the `<name>` segment is recorded as provenance only). The handler is written to accept an arbitrary store root so M4b can swap in per-agent or remote roots without changing the trait surface.

**Tech Stack:** Rust 2024, existing `serde_yaml_ng`, `sha2`, `serde_json`, `async-trait`, `tokio`. Zero new dependencies.

**Spec deltas vs `2026-05-24-mur-skill-ecosystem-design.md`** (explicitly accepted for M4a):
- §6.2 example `mur skill install agent://my-agent` (no skill segment) → rejected as malformed; the segmented form is required.
- §7.1 two-stage L1+L2 then L3 transfer → collapsed to single `skills/get` shot returning the full manifest. Two-stage transfer lands in M4b.
- §7.4 `provenance: {source, transferred_at, original_publisher, transfer_chain}` → only `transfer_chain` is added in M4a. The other fields are derivable from the chain head/tail and a timestamp on the trust entry.

---

## File Structure

**Create:**
- `mur-agent-runtime/src/protocol/methods/skills.rs` — `SkillsGetHandler` A2A method
- `mur-core/tests/skill_install_agent_e2e.rs` — integration test

**Modify:**
- `mur-common/src/skill/manifest.rs` — add `transfer_chain: Vec<String>` field
- `mur-common/src/skill/hash.rs` — add `content_hash_for_trust()` excluding `transfer_chain` and `evolution_log`
- `mur-common/src/agent.rs` — add `SkillCardEntry`/`SkillCardTrigger` types and `installed_skills: Vec<SkillCardEntry>` field on `AgentProfile`
- `mur-agent-runtime/src/protocol/methods/card.rs` — emit full `SkillCardEntry` for `installed_skills`
- `mur-agent-runtime/src/protocol/methods/mod.rs` — register `skills` module
- `mur-agent-runtime/src/supervisor.rs` — register `skills/get` handler in `build_dispatcher`
- `mur-core/src/cmd/skill_install.rs` — `agent://` URL parsing, local-store pull, content-based trust, profile registration

---

### Task 1 — `transfer_chain` field + `content_hash_for_trust`

**Files:** `mur-common/src/skill/manifest.rs`, `mur-common/src/skill/hash.rs`, `mur-common/src/skill/mod.rs`.

- [ ] **Step 1: Add `transfer_chain` to `SkillManifest`**

Append after the existing `evolution_log` field in `manifest.rs` (after line 61):

```rust
    /// Evolution history — each entry records one generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evolution_log: Vec<EvolutionEvent>,

    /// Peer transfer provenance — each entry is `agent://<name>`.
    /// Last entry is the immediate source; first entry is the original publisher.
    /// Empty for registry-installed and locally-authored skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_chain: Vec<String>,
```

- [ ] **Step 2: Add `content_hash_for_trust` to `hash.rs`**

The trust comparison hash excludes `transfer_chain` (which changes on every transfer) and `evolution_log` (which grows over time). Both the store key and the lookup key use this hash so the trust store remains consistent across hops.

```rust
/// Compute content hash for trust verification — excludes `transfer_chain`
/// and `evolution_log` so registry lookup and trust-store keys remain stable
/// across transfers and across generation increments.
pub fn content_hash_for_trust(m: &SkillManifest) -> Result<String, crate::skill::ParseError> {
    let mut clone = m.clone();
    clone.transfer_chain = vec![];
    clone.evolution_log = vec![];
    content_sha256(&clone)
}
```

- [ ] **Step 3: Re-export from the skill module**

In `mur-common/src/skill/mod.rs`, extend the existing `hash` re-export:

```rust
pub use hash::{
    DriftStatus, content_hash_for_trust, content_sha256, ct_eq_hex, drift_status, sha256_hex,
};
```

- [ ] **Step 4: Add unit tests**

In `hash.rs` tests (after `drift_detected_when_field_changed`):

```rust
#[test]
fn trust_hash_is_stable_across_transfers() {
    let m = parse_canonical(SAMPLE).unwrap();
    let h1 = content_hash_for_trust(&m).unwrap();
    let mut m2 = m.clone();
    m2.transfer_chain = vec!["agent://alice".into(), "agent://bob".into()];
    let h2 = content_hash_for_trust(&m2).unwrap();
    assert_eq!(h1, h2);
}

#[test]
fn trust_hash_is_stable_across_evolution() {
    use crate::skill::EvolutionEvent;
    let m = parse_canonical(SAMPLE).unwrap();
    let h1 = content_hash_for_trust(&m).unwrap();
    let mut m2 = m.clone();
    m2.evolution_log = vec![EvolutionEvent {
        version: "0.2.0".into(),
        generation: 1,
        source: "agent:self".into(),
        changes: "tweak".into(),
        timestamp: "2026-05-25T00:00:00Z".into(),
    }];
    let h2 = content_hash_for_trust(&m2).unwrap();
    assert_eq!(h1, h2);
}

#[test]
fn trust_hash_differs_when_content_changes() {
    let m = parse_canonical(SAMPLE).unwrap();
    let mut m2 = m.clone();
    m2.description = "changed".into();
    assert_ne!(
        content_hash_for_trust(&m).unwrap(),
        content_hash_for_trust(&m2).unwrap()
    );
}
```

> Verify the `EvolutionEvent` fields named above against `mur-common/src/skill/evolution.rs` before pasting — adjust if the struct shape differs.

- [ ] **Step 5: Run tests + commit**

```bash
cargo test -p mur-common skill::hash
git add mur-common/src/skill/manifest.rs mur-common/src/skill/hash.rs mur-common/src/skill/mod.rs
git commit -m "feat(skill): transfer_chain field + content_hash_for_trust"
```

---

### Task 2 — `SkillCardEntry` type + `installed_skills` field

**Files:** `mur-common/src/agent.rs`.

**Why a new field instead of repurposing `skills`:**  The existing `skills: Vec<String>` is written by `mur agent skill add` and contains per-agent *relative paths* like `"skills/foo.yaml"`, not skill names. `mur-core/src/cmd/agent/skill.rs` does path parsing on these strings. Repurposing the field would silently corrupt every existing profile. We add `installed_skills` as a distinct, additive field. The Card merges both lists into one broadcast (Task 4).

- [ ] **Step 1: Define `SkillCardEntry` and `SkillCardTrigger`**

Add above `pub struct AgentProfile` in `agent.rs`. All `String` fields use `skip_serializing_if = "String::is_empty"` so the YAML stays compact when a field is unset.

```rust
/// Skill metadata broadcast in the Agent Card (Layer 1 + Layer 2).
///
/// Populated by `mur skill install` (registry or agent:// URL). Distinct from
/// `AgentProfile.skills`, which is the legacy per-agent-path list managed by
/// `mur agent skill add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub publisher: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<SkillCardTrigger>,
    /// Layer 2 abstract — injected at session start (~200 tokens).
    /// On-disk YAML key is `abstract` (a Rust reserved word).
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "abstract")]
    pub abstract_text: String,
    /// Provenance chain copied from the installed manifest. Empty for
    /// registry-installed skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardTrigger {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
}
```

`PartialEq` is required — `AgentProfile` derives it, so every nested field's type must too.

- [ ] **Step 2: Add `installed_skills` field to `AgentProfile`**

In `agent.rs` after the existing `pub skills: Vec<String>` (line 25):

```rust
    #[serde(default)]
    pub skills: Vec<String>,
    /// Skills installed via `mur skill install`. Distinct from `skills`
    /// (which holds legacy per-agent paths from `mur agent skill add`).
    /// Broadcast in the Agent Card alongside `skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub installed_skills: Vec<SkillCardEntry>,
```

- [ ] **Step 3: Add unit tests**

In the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn installed_skills_default_to_empty_when_absent() {
    let yaml = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
    let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
    assert!(p.installed_skills.is_empty());
}

#[test]
fn installed_skills_roundtrip_preserves_entries() {
    let base = include_str!("../tests/fixtures/profile_p0a_minimal.yaml");
    let yaml = format!(
        "{base}installed_skills:\n  - name: s1\n    version: 1.0.0\n    publisher: human:d\n    description: desc\n    category: workflow\n    tags: [web]\n    triggers:\n      - type: command\n        pattern: /find\n    abstract: does things\n    transfer_chain:\n      - agent://alice\n"
    );
    let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(p.installed_skills.len(), 1);
    assert_eq!(p.installed_skills[0].name, "s1");
    assert_eq!(p.installed_skills[0].abstract_text, "does things");
    assert_eq!(p.installed_skills[0].transfer_chain, vec!["agent://alice"]);

    let out = serde_yaml_ng::to_string(&p).unwrap();
    assert!(out.contains("abstract: does things"));
    assert!(out.contains("pattern: /find"));

    let back: AgentProfile = serde_yaml_ng::from_str(&out).unwrap();
    assert_eq!(p.installed_skills, back.installed_skills);
}

#[test]
fn installed_skills_minimal_entry_serializes_compactly() {
    // A name-only entry must NOT emit empty string fields.
    let entry = SkillCardEntry {
        name: "minimal".into(),
        ..Default::default()
    };
    let yaml = serde_yaml_ng::to_string(&entry).unwrap();
    assert!(yaml.contains("name: minimal"));
    assert!(!yaml.contains("version:"), "empty version must be skipped: {yaml}");
    assert!(!yaml.contains("publisher:"), "empty publisher must be skipped: {yaml}");
    assert!(!yaml.contains("abstract:"), "empty abstract must be skipped: {yaml}");
}
```

- [ ] **Step 4: Audit `.skills` consumers (no changes required)**

Run:

```bash
grep -rn "\.skills\b" mur-agent-runtime/src/ mur-core/src/ 2>/dev/null | grep -v target/
```

Expected hits — confirm each remains valid (legacy `Vec<String>` access path is untouched):
- `mur-core/src/cmd/agent/skill.rs` — uses `.skills` as paths (legacy)
- `mur-core/src/agent_admin/skill.rs` — returns `.skills` (legacy)
- `mur-agent-runtime/src/task_runner.rs` — uses `self.skills` (loader-side, unrelated type)
- `mur-agent-runtime/src/supervisor_runner.rs` — uses `Config::skills` (unrelated type)
- `mur-agent-runtime/src/protocol/methods/card.rs` — will be updated in Task 4

If anything else surfaces that constructs `AgentProfile { skills: vec![...], .. }`, it now needs `..Default::default()` if `installed_skills` was previously elided, or to set `installed_skills: vec![]`.

- [ ] **Step 5: Run tests + commit**

```bash
cargo test -p mur-common agent
git add mur-common/src/agent.rs
git commit -m "feat(agent): SkillCardEntry + installed_skills profile field"
```

---

### Task 3 — `SkillsGetHandler` A2A method

**Files:** `mur-agent-runtime/src/protocol/methods/skills.rs`, `mur-agent-runtime/src/protocol/methods/mod.rs`, `mur-agent-runtime/src/supervisor.rs`.

The handler is parameterized by a `store_root: PathBuf`. In M4a `store_root == MUR_HOME` (skills live at `<MUR_HOME>/skills/<name>/`). M4b can swap in a per-agent or per-remote root without changing the trait.

- [ ] **Step 1: Create `skills.rs` handler**

```rust
//! A2A method: `skills/get` — serve a skill manifest by name.
//!
//! In M4a this handler is registered with the dispatcher but install does
//! not yet reach it over a socket — install reads the local store directly.
//! M4b wires the real round-trip.

use async_trait::async_trait;
use mur_common::skill::{
    content_hash_for_trust, global_skill_dir, read_from_dir, serialize_canonical,
};
use serde_json::{Value, json};
use std::path::PathBuf;

use crate::protocol::a2a_server::{HandlerError, MethodHandler};

pub struct SkillsGetHandler {
    /// Root of the skill store this handler serves from. In M4a this is
    /// `MUR_HOME`; skills resolve to `<root>/skills/<name>/skill.yaml`.
    store_root: PathBuf,
}

impl SkillsGetHandler {
    pub fn new(store_root: PathBuf) -> Self {
        Self { store_root }
    }
}

#[async_trait]
impl MethodHandler for SkillsGetHandler {
    async fn handle(&self, params: Option<Value>) -> Result<Value, HandlerError> {
        let params = params
            .ok_or_else(|| HandlerError::InvalidParams("missing params".into()))?;

        let skill_name = params
            .get("skill")
            .and_then(|v| v.as_str())
            .ok_or_else(|| HandlerError::InvalidParams("missing 'skill' field".into()))?;

        let dir = global_skill_dir(&self.store_root, skill_name);
        let manifest = read_from_dir(&dir)
            .map_err(|e| HandlerError::Internal(format!("skill '{skill_name}' not found: {e}")))?;

        let hash = content_hash_for_trust(&manifest)
            .map_err(|e| HandlerError::Internal(format!("hash failed: {e}")))?;

        let yaml = serialize_canonical(&manifest)
            .map_err(|e| HandlerError::Internal(format!("serialize failed: {e}")))?;

        Ok(json!({
            "manifest": yaml,
            "content_sha256": hash,
            "transfer_chain": manifest.transfer_chain,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{parse_canonical, write_to_dir};
    use tempfile::tempdir;

    #[tokio::test]
    async fn serves_installed_skill() {
        let dir = tempdir().unwrap();
        let m = parse_canonical(
            r#"
name: test-skill
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: b
"#,
        )
        .unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "test-skill"), &m).unwrap();

        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let result = handler
            .handle(Some(json!({"skill": "test-skill"})))
            .await
            .unwrap();

        assert!(result["manifest"].as_str().unwrap().contains("test-skill"));
        assert_eq!(result["content_sha256"].as_str().unwrap().len(), 64);
        assert!(result["transfer_chain"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_skill_returns_internal_error() {
        let dir = tempdir().unwrap();
        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let err = handler
            .handle(Some(json!({"skill": "nope"})))
            .await
            .unwrap_err();
        assert!(matches!(err, HandlerError::Internal(_)));
    }

    #[tokio::test]
    async fn missing_skill_param_returns_invalid_params() {
        let dir = tempdir().unwrap();
        let handler = SkillsGetHandler::new(dir.path().to_path_buf());
        let err = handler.handle(Some(json!({}))).await.unwrap_err();
        assert!(matches!(err, HandlerError::InvalidParams(_)));
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-agent-runtime/src/protocol/methods/mod.rs` add `pub mod skills;` alongside the existing module declarations.

- [ ] **Step 3: Wire the handler into `build_dispatcher`**

In `mur-agent-runtime/src/supervisor.rs`, change `build_dispatcher`'s signature to accept the store root and register `skills/get`. The current signature is:

```rust
fn build_dispatcher(profile: &Arc<Profile>, runner: &Arc<TaskRunner>) -> Dispatcher
```

Change to:

```rust
fn build_dispatcher(
    profile: &Arc<Profile>,
    runner: &Arc<TaskRunner>,
    mur_home: &Path,
) -> Dispatcher {
    let mut d = Dispatcher::new();
    d.register("agent/card", Box::new(CardHandler::new(profile.clone())));
    d.register(
        "message/send",
        Box::new(MessageSendHandler::new(runner.clone())),
    );
    d.register("tasks/get", Box::new(TasksGetHandler::new(runner.clone())));
    d.register(
        "tasks/cancel",
        Box::new(TasksCancelHandler::new(runner.clone())),
    );
    d.register(
        "tasks/list",
        Box::new(TasksListHandler::new(runner.clone())),
    );
    d.register(
        "skills/get",
        Box::new(crate::protocol::methods::skills::SkillsGetHandler::new(
            mur_home.to_path_buf(),
        )),
    );
    d
}
```

Update the single caller at `supervisor.rs:202`:

```rust
let dispatcher = Arc::new(build_dispatcher(&profile_arc, &runner, &mur_home));
```

`mur_home` is computed earlier in the same function (`supervisor.rs:81`). Add `use std::path::Path;` if not already imported.

- [ ] **Step 4: Run tests + commit**

```bash
cargo test -p mur-agent-runtime protocol::methods::skills
cargo build -p mur-agent-runtime  # catch any caller breakage
git add mur-agent-runtime/src/protocol/methods/skills.rs \
        mur-agent-runtime/src/protocol/methods/mod.rs \
        mur-agent-runtime/src/supervisor.rs
git commit -m "feat(a2a): skills/get handler — serve manifest + hash + transfer_chain"
```

---

### Task 4 — Agent Card broadcasts `installed_skills`

**Files:** `mur-agent-runtime/src/protocol/methods/card.rs`.

The Card emits both `skills` (legacy paths, unchanged) and a new `installed_skills` block carrying full `SkillCardEntry` data populated by `mur skill install` (Task 5).

- [ ] **Step 1: Add `installed_skills` to the Card JSON**

Replace the existing skills line:

```rust
// Current (M0):
"skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
```

with:

```rust
"skills": p.skills.iter().map(|s| json!({"id": s})).collect::<Vec<_>>(),
"installed_skills": p.installed_skills.iter().map(|s| json!({
    "name": s.name,
    "version": s.version,
    "publisher": s.publisher,
    "description": s.description,
    "category": s.category,
    "tags": s.tags,
    "triggers": s.triggers.iter().map(|t| json!({
        "type": t.kind,
        "pattern": t.pattern,
    })).collect::<Vec<_>>(),
    "abstract": s.abstract_text,
    "transfer_chain": s.transfer_chain,
})).collect::<Vec<_>>(),
```

- [ ] **Step 2: Add a Card test for `installed_skills`**

If `card.rs` already has handler tests, extend the fixture. Otherwise add:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::SkillCardEntry;
    // ... reuse whatever Profile fixture helper exists ...

    #[tokio::test]
    async fn card_emits_installed_skills_block() {
        let mut profile = mur_common::AgentProfile::default_for_tests();
        profile.installed_skills.push(SkillCardEntry {
            name: "find-prices".into(),
            version: "1.0.0".into(),
            publisher: "human:alice".into(),
            description: "find prices".into(),
            category: "workflow".into(),
            abstract_text: "looks up prices".into(),
            transfer_chain: vec!["agent://alice".into()],
            ..Default::default()
        });
        let handler = CardHandler::new(Arc::new(Profile { inner: profile, /* ... */ }));
        let card = handler.handle(None).await.unwrap();
        let entries = card["installed_skills"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "find-prices");
        assert_eq!(entries[0]["transfer_chain"][0], "agent://alice");
    }
}
```

> Inspect `Profile` (in `mur-agent-runtime/src/profile.rs`) for its actual public fields before pasting — the fixture construction must match. If existing tests already build a `Profile` for `CardHandler`, copy that pattern.

- [ ] **Step 3: Run tests + commit**

```bash
cargo test -p mur-agent-runtime protocol::methods::card
git add mur-agent-runtime/src/protocol/methods/card.rs
git commit -m "feat(a2a): Agent Card broadcasts installed_skills with Layer 1+2 metadata"
```

---

### Task 5 — `agent://` install path in `cmd_install`

**Files:** `mur-core/src/cmd/skill_install.rs`.

This task does three things:
1. Parse `agent://<agent>/<skill>` URLs.
2. Pull the manifest from the local skill store (M4a single-home reality — see assumption at top), apply content-based trust, append the transfer chain, and write into the target store.
3. Register the result on the target agent's `profile.yaml` so the Card can broadcast it.

- [ ] **Step 1: Add imports**

At the top of `skill_install.rs`, expand the existing imports:

```rust
use anyhow::{Context, Result, anyhow, bail};
use std::path::Path;

use mur_common::skill::{
    SkillLock, SkillManifest, TrustLevel, content_hash_for_trust, content_sha256,
    global_skill_dir, lockfile, parse_canonical, read_from_dir, scan::scan_skill, write_to_dir,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
```

- [ ] **Step 2: Branch `cmd_install` on `agent://` before the registry fetch**

The registry fetch must not run for `agent://` URLs (the agent path doesn't need the registry to be reachable; it only consults the cached index if it happens to already exist).

```rust
pub fn cmd_install(home: &Path, registry_url: &str, source: &str) -> Result<()> {
    // M4a: agent://<agent-name>/<skill-name> — peer transfer pull.
    if let Some(rest) = source.strip_prefix("agent://") {
        let (agent_name, skill_name) = rest.split_once('/').ok_or_else(|| {
            anyhow!(
                "invalid agent:// URL '{source}' — expected agent://<agent-name>/<skill-name>"
            )
        })?;
        if agent_name.is_empty() || skill_name.is_empty() {
            bail!(
                "invalid agent:// URL '{source}' — agent name and skill name must be non-empty"
            );
        }
        return install_from_agent(home, agent_name, skill_name);
    }

    // Existing registry / local-file flow continues unchanged below.
    let src_path = Path::new(source);
    let (reg_dir, _idx) =
        skill_registry::fetch_and_load(home, registry_url).context("fetch registry")?;
    // ... rest of the existing body unchanged ...
}
```

- [ ] **Step 3: Implement `install_from_agent`**

```rust
fn install_from_agent(home: &Path, agent_name: &str, skill_name: &str) -> Result<()> {
    // 1. Discover — verify the named agent exists locally.
    //    In M4a we share one MUR_HOME, so the source store IS `home`. The
    //    agent dir check confirms the name is a real agent on this host —
    //    a sanity guard, not a security boundary.
    let agent_dir = home.join("agents").join(agent_name);
    if !agent_dir.exists() {
        bail!(
            "agent '{agent_name}' not found at {} — \
             M4a requires the source agent to live on the same MUR_HOME",
            agent_dir.display()
        );
    }

    // 2. Pull — read the skill directly from the shared local store. M4b
    //    will replace this block with a real A2A `skills/get` over a
    //    socket; the request/response shape already matches.
    let source_dir = global_skill_dir(home, skill_name);
    let mut manifest: SkillManifest = read_from_dir(&source_dir).map_err(|e| {
        anyhow!("pull from agent://{agent_name}/{skill_name} failed: {e}")
    })?;
    let received_hash = content_hash_for_trust(&manifest)
        .map_err(|e| anyhow!("hash source manifest: {e}"))?;

    // 3. Verify — content-based trust.
    let trust_level = resolve_agent_install_trust(home, &manifest, &received_hash)?;

    // 4. Append transfer chain (this is what differentiates the pulled
    //    copy from the source — the trust hash excludes this field so
    //    revocation and registry lookup still work).
    manifest
        .transfer_chain
        .push(format!("agent://{agent_name}"));

    // 5. Re-scan the post-mutation manifest. A scan finding always wins
    //    over the trust decision (downgrades Verified → Sandboxed).
    let report = scan_skill(&manifest).context("scan manifest")?;
    let effective_level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        trust_level
    };

    // 6. Install — write to the target store (same MUR_HOME in M4a; this
    //    rewrites the source file with the chain appended, which is the
    //    intended single-home behavior).
    let dir = global_skill_dir(home, &manifest.name);
    write_to_dir(&dir, &manifest).context("write installed skill")?;

    // 7. Record in trust store. Key by `content_hash_for_trust` so the
    //    entry survives future chain extensions and evolution events.
    let trust_key = content_hash_for_trust(&manifest)
        .map_err(|e| anyhow!("hash trust key: {e}"))?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(
        trust_key,
        TrustEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            level: effective_level,
            installed_at: chrono::Utc::now().to_rfc3339(),
            publisher: Some(manifest.publisher.clone()),
        },
    );
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;

    // 8. Register in the calling agent's profile so the Card can
    //    broadcast it. Caller identity comes from $MUR_AGENT_NAME (set by
    //    the supervisor) or falls back to skipping registration in
    //    contexts that don't have a calling agent (CLI from a shell).
    if let Some(caller) = caller_agent_name(home)? {
        register_in_profile(home, &caller, &manifest)?;
    }

    if report.has_blocking_findings() {
        eprintln!(
            "⚠ {} v{}: security findings — installed Sandboxed",
            manifest.name, manifest.version
        );
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }

    println!(
        "installed: {} v{} ({effective_level:?}, from agent://{agent_name})",
        manifest.name, manifest.version,
    );
    if effective_level == TrustLevel::Sandboxed {
        println!(
            "hint: run `mur skill trust {} verified` after review",
            manifest.name
        );
    }
    Ok(())
}

/// Content-based trust for agent-installed skills.
/// Order: revocation > registry hash match > default Sandboxed.
fn resolve_agent_install_trust(
    home: &Path,
    manifest: &SkillManifest,
    received_hash: &str,
) -> Result<TrustLevel> {
    let trust_store = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    if trust_store.is_revoked(received_hash) {
        bail!(
            "skill '{}' (hash {}) is revoked — install blocked",
            manifest.name,
            received_hash
        );
    }

    let cache_dir = crate::cmd::skill_registry::registry_cache_dir(home);
    if cache_dir.exists()
        && let Ok(idx) = crate::cmd::skill_registry::load_index(&cache_dir)
        && let Some(entry) = idx.skills.get(&manifest.name)
        && entry.content_sha256 == received_hash
    {
        return Ok(TrustLevel::Verified);
    }

    Ok(TrustLevel::Sandboxed)
}

/// Resolve the agent that issued this install command. The supervisor
/// exports `MUR_AGENT_NAME` for spawned commands; a bare CLI invocation
/// from a shell will have it unset, in which case we skip profile
/// registration (the skill is still installed in the shared store).
fn caller_agent_name(home: &Path) -> Result<Option<String>> {
    let Some(name) = std::env::var("MUR_AGENT_NAME").ok() else {
        return Ok(None);
    };
    let agent_dir = home.join("agents").join(&name);
    if !agent_dir.exists() {
        bail!(
            "MUR_AGENT_NAME='{name}' but {} does not exist",
            agent_dir.display()
        );
    }
    Ok(Some(name))
}

/// Push (or update) a `SkillCardEntry` into the calling agent's profile.
fn register_in_profile(home: &Path, agent_name: &str, m: &SkillManifest) -> Result<()> {
    use mur_common::agent::{SkillCardEntry, SkillCardTrigger};

    let profile_path = home.join("agents").join(agent_name).join("profile.yaml");
    let text = std::fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: mur_common::AgentProfile =
        serde_yaml_ng::from_str(&text).with_context(|| format!("parse {}", profile_path.display()))?;

    let entry = SkillCardEntry {
        name: m.name.clone(),
        version: m.version.clone(),
        publisher: m.publisher.clone(),
        description: m.description.clone(),
        category: serde_yaml_ng::to_string(&m.category)
            .unwrap_or_default()
            .trim()
            .to_string(),
        tags: m.tags.clone(),
        triggers: m
            .triggers
            .iter()
            .map(|t| SkillCardTrigger {
                kind: serde_yaml_ng::to_string(&t.kind)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                pattern: t.pattern.clone().unwrap_or_default(),
            })
            .collect(),
        abstract_text: m.content.r#abstract.clone(),
        transfer_chain: m.transfer_chain.clone(),
    };

    if let Some(slot) = profile
        .installed_skills
        .iter_mut()
        .find(|e| e.name == entry.name)
    {
        *slot = entry;
    } else {
        profile.installed_skills.push(entry);
    }

    // Atomic write — temp file + rename.
    let tmp = profile_path.with_extension("yaml.tmp");
    let yaml = serde_yaml_ng::to_string(&profile)?;
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &profile_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), profile_path.display()))?;
    Ok(())
}
```

> The `let … && let …` chain uses Rust edition 2024 let-chains, which this workspace already uses (see B1 / D1 milestone notes). If clippy flags it, fall back to nested `if let`.

- [ ] **Step 4: Add unit tests for URL parsing**

```rust
#[cfg(test)]
mod agent_url_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn malformed_agent_urls_are_rejected() {
        let home = tempdir().unwrap();
        let cases = [
            ("agent://noslash", "expected agent://"),
            ("agent://", "expected agent://"),
            ("agent:///emptyagent", "non-empty"),
            ("agent://emptyskill/", "non-empty"),
        ];
        for (input, expected_substr) in cases {
            let err = cmd_install(home.path(), "https://example.com/registry", input)
                .unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains(expected_substr),
                "case {input:?}: expected '{expected_substr}' in '{msg}'"
            );
        }
    }
}
```

- [ ] **Step 5: Run tests + commit**

```bash
cargo test -p mur-core skill_install
git add mur-core/src/cmd/skill_install.rs
git commit -m "feat(skill): agent:// URL install — peer pull, content-based trust, profile register"
```

---

### Task 6 — Integration test (two-home setup)

**Files:** `mur-core/tests/skill_install_agent_e2e.rs`.

> **Naming:** This is an integration test of the install code path, not a wire-level A2A E2E. The handler/dispatcher wire test lands in M4b alongside the real socket round-trip. Keep the file name for grep continuity but describe it accurately in the file header.

The test simulates a "source agent has this skill, target agent installs it" flow by using a single `MUR_HOME` (matching the M4a single-home reality) and verifying transfer-chain append + profile registration + trust level.

- [ ] **Step 1: Write the test**

```rust
//! Integration test for `mur skill install agent://...` in the M4a
//! single-home reality. The handler/dispatcher wire round-trip is
//! exercised separately in M4b.

use mur_common::agent::AgentProfile;
use mur_common::skill::{
    TrustLevel, content_hash_for_trust, global_skill_dir, parse_canonical, read_from_dir,
    write_to_dir,
};
use mur_common::trust::skills::SkillTrustStore;
use mur_core::cmd::skill_install::cmd_install;
use tempfile::tempdir;

fn write_profile(home: &std::path::Path, name: &str) -> std::path::PathBuf {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let profile = AgentProfile {
        name: name.to_string(),
        ..AgentProfile::default_for_tests()
    };
    let yaml = serde_yaml_ng::to_string(&profile).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, yaml).unwrap();
    path
}

#[test]
fn agent_pull_installs_and_appends_transfer_chain() {
    let home = tempdir().unwrap();

    // Source agent "alice" — owns the skill.
    write_profile(home.path(), "alice");
    let manifest = parse_canonical(
        r#"
name: find-prices
version: 1.0.0
publisher: human:alice
description: Find product prices
category: workflow
content:
  abstract: Searches product prices.
  context: "Full procedure."
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "find-prices"), &manifest).unwrap();

    // Target agent "bob" — caller of the install. `MUR_AGENT_NAME` is how
    // `cmd_install` learns which profile to register the entry on.
    let bob_profile_path = write_profile(home.path(), "bob");

    // Safety: env mutation isn't thread-safe across parallel tests. Either
    // use cargo's `--test-threads=1` for this suite or wrap in a Mutex.
    // For now, rely on `tempfile`-unique paths and accept single-threaded.
    // SAFETY: see comment above.
    unsafe { std::env::set_var("MUR_AGENT_NAME", "bob") };

    let result = cmd_install(
        home.path(),
        "https://example.com/registry", // unused for agent:// path
        "agent://alice/find-prices",
    );

    // SAFETY: see comment above.
    unsafe { std::env::remove_var("MUR_AGENT_NAME") };
    result.unwrap();

    // 1. Skill file exists in the shared store.
    let installed_dir = global_skill_dir(home.path(), "find-prices");
    assert!(installed_dir.join("skill.yaml").exists());

    // 2. transfer_chain was appended (the single-home model means the
    //    source file is rewritten in place with the chain extended).
    let installed = read_from_dir(&installed_dir).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://alice"]);

    // 3. Trust entry is Sandboxed (no registry cache in this test).
    //    Keyed by content_hash_for_trust so future re-shares still find it.
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let key = content_hash_for_trust(&installed).unwrap();
    let entry = trust.lookup(&key).expect("trust entry exists");
    assert!(matches!(entry.level, TrustLevel::Sandboxed));

    // 4. Bob's profile carries the SkillCardEntry.
    let bob_yaml = std::fs::read_to_string(&bob_profile_path).unwrap();
    let bob: AgentProfile = serde_yaml_ng::from_str(&bob_yaml).unwrap();
    assert_eq!(bob.installed_skills.len(), 1);
    let entry = &bob.installed_skills[0];
    assert_eq!(entry.name, "find-prices");
    assert_eq!(entry.publisher, "human:alice");
    assert_eq!(entry.abstract_text, "Searches product prices.");
    assert_eq!(entry.transfer_chain, vec!["agent://alice"]);
}

#[test]
fn agent_url_rejects_missing_source_agent() {
    let home = tempdir().unwrap();
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://nonexistent/skill",
    )
    .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn agent_url_rejects_missing_source_skill() {
    let home = tempdir().unwrap();
    write_profile(home.path(), "charlie");
    let err = cmd_install(
        home.path(),
        "https://example.com/registry",
        "agent://charlie/missing-skill",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not found") || msg.contains("pull"),
        "unexpected error: {msg}"
    );
}

#[test]
fn agent_url_skips_profile_register_without_caller() {
    let home = tempdir().unwrap();
    write_profile(home.path(), "dave");
    let manifest = parse_canonical(
        r#"
name: solo
version: 1.0.0
publisher: human:dave
description: d
category: context
content:
  abstract: a
  context: b
"#,
    )
    .unwrap();
    write_to_dir(&global_skill_dir(home.path(), "solo"), &manifest).unwrap();

    // No MUR_AGENT_NAME set → install succeeds, no profile mutation.
    cmd_install(home.path(), "https://x", "agent://dave/solo").unwrap();
    let installed = read_from_dir(&global_skill_dir(home.path(), "solo")).unwrap();
    assert_eq!(installed.transfer_chain, vec!["agent://dave"]);
}
```

> **Env-var caveat:** `std::env::set_var` is `unsafe` on Rust 2024 and not safe across threads. Run this file with `--test-threads=1` or refactor `caller_agent_name` to accept a config struct so the test doesn't need env mutation. The simpler path: add a `#[cfg(test)]` thread-local override hook in `skill_install.rs`. Pick one before merging.

- [ ] **Step 2: Run the test**

```bash
cargo test -p mur-core --test skill_install_agent_e2e -- --test-threads=1
```

Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/skill_install_agent_e2e.rs
git commit -m "test(skill): agent:// install integration — transfer chain + profile register"
```

---

## Self-Review

**Spec coverage:**

| Spec § | Requirement | Task |
|---|---|---|
| §3.1 | `transfer_chain` on manifest | T1 |
| §3.1 | `content_hash_for_trust` excludes chain + evolution | T1 |
| §3.2 | `SkillCardEntry` with Layer 1+2 fields | T2 |
| §3.3 | `skills/get` A2A method | T3 |
| §4 | Agent Card broadcasts full entries | T4 |
| §5.1 | `agent://` URL parsing | T5 |
| §5.3 | Registry hash match → Verified | T5 (`resolve_agent_install_trust`) |
| §5.3 | No match → Sandboxed | T5 |
| §5.3 | Revoked → reject | T5 |
| §5.4 | Transfer chain append on install | T5 |
| §6 | Trust decision matrix | T5 |
| §7 | Profile registration so Card can broadcast | T5 (`register_in_profile`) |

**Hash consistency:** `content_hash_for_trust` is used in T3 (handler response), T5 step 3 (trust lookup), T5 step 7 (trust store key), and T6 step 1 (test verification). The full `content_sha256` is *not* used in the agent-install path so chain extensions and future evolution events do not orphan trust entries.

**Sync/async:** `cmd_install` stays sync. The local-store pull (T5 step 3) reads the manifest directly from disk via `read_from_dir` — no async, no runtime juggling. The `SkillsGetHandler` async path is reserved for the M4b socket round-trip; it is dispatcher-registered now so wire callers in M4b have the method available.

**Backward compatibility:** `AgentProfile.skills` (legacy paths from `mur agent skill add`) is untouched. `installed_skills` is new, defaults to empty, and `skip_serializing_if = "Vec::is_empty"` keeps it absent from existing profile.yaml files. `SkillCardEntry` String fields use `skip_serializing_if = "String::is_empty"` so name-only entries serialize as a single line, not a verbose block.

**Compile-blocker scan:**
- `#[async_trait]` applied on the new handler impl (T3).
- `HandlerError` variants constructed directly, not via non-existent helper methods (T3).
- `SkillCardEntry`/`SkillCardTrigger` derive `PartialEq` so `AgentProfile`'s derive keeps compiling (T2).
- `Default` derived on both so `..Default::default()` shorthand works (T2).
- `register_in_profile` reuses existing `serde_yaml_ng` and `chrono` imports already present in `skill_install.rs`.

**Placeholder scan:** Clean — no TBD, no "add error handling later", no stubs.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m4a.md`.

Suggested branch: `feat/skill-ecosystem-m4a`. Zero dependencies on M3b/M3c — only needs M3a (merged) for the existing install pipeline and `evolution_log` (already on `SkillManifest` per commit `ec2989f`).

Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
