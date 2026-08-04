# Memory Federation P0+T0 — signed snapshot pull, skill content, tracer bullet

> **Execution skill:** `mur-executing-plans` (single writer, sequential tasks).
> Spec: `docs/superpowers/specs/2026-08-04-unified-memory-federation.md` (P0 + T0 rows).

**Goal:** replace the dead Pattern-era snapshot pull with a daemon-verified, Ed25519-signed
request/response that delivers lifecycle-filtered Skills into a per-agent cache the runtime
injects — proven end-to-end by an automated smoke test plus the T0 manual tracer.

**Architecture:** the agent runtime drops a signed `SnapshotRequest` file into
`~/.mur/inbox/snapshot-requests/` (same file-drop pattern its outbox already uses). The
daemon — outside every agent sandbox — polls that directory, verifies the signature against
the agent's on-disk pubkey, assembles the skill snapshot central-side into
`agents/<name>/knowledge_cache/`, and deletes the request. The shared skill loader gains
`knowledge_cache` as a source, so the existing injector picks cached skills up with no
runtime prompt changes.

**Tech stack:** Rust (edition 2024), serde_yaml_ng, chrono, ed25519-dalek via
`mur_common::identity`, multibase (Base58Btc), tokio (daemon task).

## Global Constraints (verbatim, every task)

- **No hardcoded values.** Poll interval, freshness window, lifecycle floor: all from
  `Config` with serde defaults (CLAUDE.md rule 1).
- **No `mur` in any agent spawn allowlist.** The subprocess pull is being deleted; do not
  reintroduce it.
- **New runtime code reads only files inside the agent home** (`agents/<name>/…`) and
  writes only there plus `~/.mur/inbox/…` (the grant the outbox flush already uses).
- **Single writer:** only daemon/CLI code (mur-core) writes `knowledge_cache`; the runtime
  never writes it.
- **Canonical sign-input excludes the `sig` field** (v3d `ChannelEvent` precedent).
- **Atomic writes:** temp file + rename (house idiom, `store/yaml.rs`).
- **Path-traversal guard:** any agent name arriving from a request file is validated
  `[A-Za-z0-9_-]+` before being joined into a path.
- Tests run with `cargo nextest` (never bare `cargo test --workspace`). Env recipe for
  every build/test command:
  `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUSTFLAGS=-Cdebuginfo=0`
- Source files stay ≤ 800 lines.

## File structure

| file | change | responsibility |
|---|---|---|
| `mur-common/src/identity.rs` | modify | add `sign_multibase` (encode half of existing `verify_bytes`) |
| `mur-common/src/snapshot_request.rs` | **new** | `SnapshotRequest` type: create/verify/freshness + dir constant |
| `mur-common/src/lib.rs` | modify | `pub mod snapshot_request;` |
| `mur-common/src/config.rs` | modify | `SnapshotConfig` block (`federation_snapshot:`) |
| `mur-core/src/federation/snapshot.rs` | modify | `assemble_skill_snapshot` (skills → knowledge_cache); retire Pattern body |
| `mur-core/src/cmd/agent/snapshot.rs` | modify | `mur agent snapshot pull/show` re-pointed at skill snapshots |
| `mur-daemon/src/snapshot_requests.rs` | **new** | poll → validate → verify → assemble → consume |
| `mur-daemon/src/main.rs` | modify | one `snapshot_requests::spawn(...)` line |
| `mur-agent-runtime/src/federation/sync.rs` | modify | `refresh_snapshot` = signed request drop; delete subprocess + zombie reaper |
| `mur-common/src/skill/loader.rs` | modify | `load_all` scans `knowledge_cache` (precedence: agent-local > cache > global) |

---

## Task 1 — `SnapshotRequest` + `sign_multibase` (mur-common)

**Interfaces — Produces:**
- `AgentIdentity::sign_multibase(&self, msg: &[u8]) -> String`
- `mur_common::snapshot_request::{SnapshotRequest, SNAPSHOT_REQUEST_DIR}`
  - `SnapshotRequest::create(agent: &str, identity: &AgentIdentity, now: DateTime<Utc>) -> Self`
  - `SnapshotRequest::verify(&self, pubkey: &[u8; 32]) -> bool`
  - `SnapshotRequest::is_fresh(&self, now: DateTime<Utc>, max_age_secs: u64) -> bool`

Steps:

- [x] In `mur-common/src/identity.rs`, directly under `sign_bytes` (line ~108), add:

```rust
    /// Sign `msg` and encode the signature as multibase Base58Btc — the exact
    /// encoding `verify_bytes` decodes (mirrors mur-channel/src/sign.rs:53).
    pub fn sign_multibase(&self, msg: &[u8]) -> String {
        multibase::encode(multibase::Base::Base58Btc, self.sign_bytes(msg))
    }
```

- [x] Write the failing test first — new file `mur-common/src/snapshot_request.rs` with ONLY
      the test module below plus empty stubs; watch it fail to compile / fail assertions:

```rust
//! Signed snapshot-pull request — the agent-side half of the memory-federation
//! pull leg (spec: docs/superpowers/specs/2026-08-04-unified-memory-federation.md).
//! An agent runtime writes one (YAML, tmp+rename) into
//! `<mur_home>/inbox/snapshot-requests/`; the daemon verifies it against the
//! agent's on-disk pubkey and assembles the snapshot central-side.
//! Canonical sign-input EXCLUDES `sig` (v3d ChannelEvent precedent).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{AgentIdentity, verify_bytes};

/// File-drop directory, relative to the MUR home.
pub const SNAPSHOT_REQUEST_DIR: &str = "inbox/snapshot-requests";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRequest {
    pub agent: String,
    pub requested_at: DateTime<Utc>,
    /// Key-rotation version; 0 = initial identity key. Recorded for forward
    /// compatibility — P0 verifies against the CURRENT pubkey only.
    #[serde(default)]
    pub key_version: u32,
    /// Multibase (Base58Btc) Ed25519 signature over the canonical sign-input.
    pub sig: String,
}

/// Canonical signed bytes: domain tag + fields, `sig` excluded.
fn sign_input(agent: &str, requested_at: &DateTime<Utc>, key_version: u32) -> Vec<u8> {
    format!(
        "mur-snapshot-request-v1\n{agent}\n{}\n{key_version}",
        requested_at.to_rfc3339()
    )
    .into_bytes()
}

impl SnapshotRequest {
    pub fn create(agent: &str, identity: &AgentIdentity, now: DateTime<Utc>) -> Self {
        let input = sign_input(agent, &now, 0);
        Self {
            agent: agent.to_string(),
            requested_at: now,
            key_version: 0,
            sig: identity.sign_multibase(&input),
        }
    }

    /// Fail-closed signature check against `pubkey`.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        let input = sign_input(&self.agent, &self.requested_at, self.key_version);
        verify_bytes(pubkey, &input, &self.sig)
    }

    /// Inside the acceptance window? Blunts replay; not a nonce store —
    /// consuming the request file on processing is the other half.
    pub fn is_fresh(&self, now: DateTime<Utc>, max_age_secs: u64) -> bool {
        let age = now.signed_duration_since(self.requested_at);
        age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(max_age_secs as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> AgentIdentity {
        AgentIdentity::generate()
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = identity();
        let req = SnapshotRequest::create("dr_worker_4", &id, Utc::now());
        assert!(req.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn tampered_agent_name_fails_verification() {
        let id = identity();
        let mut req = SnapshotRequest::create("dr_worker_4", &id, Utc::now());
        req.agent = "dr_worker_1".into(); // impersonation attempt
        assert!(!req.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn wrong_key_fails_verification() {
        let req = SnapshotRequest::create("a", &identity(), Utc::now());
        assert!(!req.verify(&identity().verifying_key_bytes()));
    }

    #[test]
    fn freshness_window_rejects_old_and_future() {
        let id = identity();
        let now = Utc::now();
        let req = SnapshotRequest::create("a", &id, now);
        assert!(req.is_fresh(now, 600));
        assert!(!req.is_fresh(now + chrono::Duration::seconds(601), 600)); // stale
        assert!(!req.is_fresh(now - chrono::Duration::seconds(1), 600)); // future-dated
    }

    #[test]
    fn yaml_roundtrip_preserves_signature() {
        let id = identity();
        let req = SnapshotRequest::create("a", &id, Utc::now());
        let yaml = serde_yaml_ng::to_string(&req).unwrap();
        let back: SnapshotRequest = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(back.verify(&id.verifying_key_bytes()));
    }
}
```

- [x] Register the module: in `mur-common/src/lib.rs`, alongside the existing `pub mod
      identity;`, add `pub mod snapshot_request;`.
- [x] Run and watch pass:
      `cargo nextest run -p mur-common -E 'test(snapshot_request)'`
      Expected: `5 tests run: 5 passed`.
- [x] Commit: `feat(common): signed SnapshotRequest for the federation pull leg`

---

## Task 2 — `SnapshotConfig` (mur-common/src/config.rs)

**Interfaces — Produces:** `Config.federation_snapshot: SnapshotConfig` with fields
`poll_secs: u64` (default 30), `request_max_age_secs: u64` (default 600),
`min_lifecycle: LifecycleState` (default `Stable`).

Steps:

- [x] Follow the `ParallelJobsConfig` precedent in `mur-common/src/config.rs`
      (around line 183). Add next to it:

```rust
// --- memory-federation snapshot (spec 2026-08-04-unified-memory-federation) ---

/// Daemon-side settings for the signed snapshot pull. Stored under
/// `federation_snapshot:` in `~/.mur/config.yaml`; every field has a default
/// so an absent block means "defaults", never "off".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SnapshotConfig {
    /// How often the daemon sweeps `inbox/snapshot-requests/`, in seconds.
    pub poll_secs: u64,
    /// Reject requests older than this (replay blunting), in seconds.
    pub request_max_age_secs: u64,
    /// Minimum lifecycle state a global skill needs to enter a snapshot.
    pub min_lifecycle: crate::skill::stats::LifecycleState,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self {
            poll_secs: 30,
            request_max_age_secs: 600,
            min_lifecycle: crate::skill::stats::LifecycleState::Stable,
        }
    }
}
```

      and the field on `Config` (same shape as `parallel_jobs`):

```rust
    #[serde(default)]
    pub federation_snapshot: SnapshotConfig,
```

- [x] Test in the existing config test module:

```rust
    #[test]
    fn federation_snapshot_defaults_apply_when_block_absent() {
        let c: Config = serde_yaml_ng::from_str("{}").unwrap();
        assert_eq!(c.federation_snapshot.poll_secs, 30);
        assert_eq!(c.federation_snapshot.request_max_age_secs, 600);
        assert_eq!(
            c.federation_snapshot.min_lifecycle,
            crate::skill::stats::LifecycleState::Stable
        );
    }
```

      (If `LifecycleState` lacks `Serialize`/`Deserialize`/`PartialEq` derives, add the
      missing ones to its derive list in `stats.rs` — additive, no behavior change.)
- [x] `cargo nextest run -p mur-common -E 'test(federation_snapshot)'` → `1 passed`.
- [x] **Workspace-literal check** (a new field on a shared mur-common type breaks struct
      literals in other members and EXCLUDED Tauri crates):
      `cargo test --workspace --no-run 2>&1 | tail -5` must end in a successful build.
      `#[serde(default)]` covers YAML; fix any `Config { .. }` literals the compiler names.
- [x] Commit: `feat(common): federation_snapshot config block`

---

## Task 3 — skill snapshot assembly (mur-core)

**Interfaces — Consumes:** `SnapshotConfig` (Task 2), `SkillStats::path(mur_home, name)`,
`mur_common::skill::local::list_installed(mur_home)`.
**Produces:** `mur_core::federation::assemble_skill_snapshot(mur_home: &Path, agent_name:
&str) -> Result<SkillSnapshotRef>`; cache layout
`agents/<name>/knowledge_cache/<skill>/skill.yaml` + `knowledge_cache/.snapshot-ref`.

Steps:

- [x] In `mur-core/src/federation/snapshot.rs`, add (Pattern-era `pull_snapshot`,
      `apply_filter`, and `PatternFilter` plumbing get deleted in the last step of this
      task — write the new code first so the file never loses its only content):

```rust
/// What one assembly wrote. Serialized as `knowledge_cache/.snapshot-ref`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SkillSnapshotRef {
    pub skill_count: usize,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Central-side skill snapshot: copy every GLOBAL skill at or above the
/// configured lifecycle floor into
/// `agents/<agent_name>/knowledge_cache/<skill>/skill.yaml`.
/// Replaces the removed Pattern snapshot; Notes join in federation P1.
/// Per-agent skills are NOT copied — they already live in the agent home and
/// win name collisions in the loader.
pub fn assemble_skill_snapshot(
    mur_home: &Path,
    agent_name: &str,
) -> Result<SkillSnapshotRef> {
    use mur_common::skill::stats::SkillStats;

    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let floor = cfg.federation_snapshot.min_lifecycle;

    let cache = mur_home
        .join("agents")
        .join(agent_name)
        .join("knowledge_cache");
    // Rebuild from scratch each pull: a skill demoted below the floor (or
    // deleted centrally) must disappear from the cache, and stale-file
    // removal by diffing is more code than a clean rebuild of a small dir.
    if cache.exists() {
        std::fs::remove_dir_all(&cache)
            .with_context(|| format!("clear knowledge_cache for {agent_name}"))?;
    }
    std::fs::create_dir_all(&cache)?;

    let mut count = 0usize;
    for name in mur_common::skill::local::list_installed(mur_home)? {
        let stats = SkillStats::load(&SkillStats::path(mur_home, &name))?
            .unwrap_or_default();
        // Lifecycle ordering is the existing rank (lifecycle.rs:268): a skill
        // qualifies when its state ranks at or above the configured floor.
        if mur_common::skill::lifecycle::lifecycle_rank(stats.state)
            < mur_common::skill::lifecycle::lifecycle_rank(floor)
        {
            continue;
        }
        let src = mur_home.join("skills").join(&name).join("skill.yaml");
        if !src.exists() {
            continue; // run-ledger dirs and other non-skill entries
        }
        let dst_dir = cache.join(&name);
        std::fs::create_dir_all(&dst_dir)?;
        // tmp+rename so a crashed assembly never leaves a half-written yaml.
        let tmp = dst_dir.join(".skill.yaml.tmp");
        std::fs::copy(&src, &tmp)?;
        std::fs::rename(&tmp, dst_dir.join("skill.yaml"))?;
        count += 1;
    }

    let snap = SkillSnapshotRef {
        skill_count: count,
        taken_at: chrono::Utc::now(),
    };
    let yaml = serde_yaml_ng::to_string(&snap)?;
    let tmp = cache.join(".snapshot-ref.tmp");
    std::fs::write(&tmp, &yaml)?;
    std::fs::rename(&tmp, cache.join(".snapshot-ref"))?;

    // The Pattern-era cache is dead (patterns removed in workflow-engine v2
    // P1a/P1b); remove it when empty, leave it with a warning otherwise.
    let old = mur_home.join("agents").join(agent_name).join("patterns_cache");
    if old.exists() {
        match std::fs::read_dir(&old).map(|mut d| d.next().is_none()) {
            Ok(true) => {
                let _ = std::fs::remove_dir(&old);
            }
            _ => tracing::warn!(
                agent = agent_name,
                "patterns_cache is non-empty; leaving it (patterns are retired)"
            ),
        }
    }
    Ok(snap)
}
```

      Prerequisite inside this step: the rank function at
      `mur-common/src/skill/lifecycle.rs:268` is module-private today — make it
      `pub fn lifecycle_rank(state: LifecycleState) -> u8` (rename to this exact name if
      it differs) and re-export nothing else. Both call sites above use it; do not copy
      the match arms into mur-core — duplicate rank tables drifting apart is exactly the
      cross-task bug class this plan's self-review checks for.

- [x] Tests (same file, `#[cfg(test)]`), each with a tempdir `mur_home` fixture that
      writes `skills/<name>/skill.yaml` (any minimal valid manifest YAML) and a stats
      file via `SkillStats::path`:
  - `assemble_filters_below_floor`: one Stable + one Draft skill → cache holds exactly
    the Stable one; `.snapshot-ref.skill_count == 1`.
  - `assemble_rebuild_drops_demoted`: assemble with a Stable skill; demote its stats to
    Draft; assemble again → cache no longer contains it.
  - `assemble_removes_empty_patterns_cache`: pre-create empty `patterns_cache/` →
    gone after assembly.
- [x] Watch them fail, implement, watch pass:
      `cargo nextest run -p mur-core -E 'test(assemble_)'` → `3 passed`.
- [x] Re-point the CLI (`mur-core/src/cmd/agent/snapshot.rs`): `cmd_snapshot_pull` calls
      `assemble_skill_snapshot` (drop the `PatternFilter` load; `--dry-run` lists the
      skills that WOULD be copied with their lifecycle state, using the same floor);
      `cmd_snapshot_show` prints the `.snapshot-ref` from `knowledge_cache`. Delete the
      now-unreferenced Pattern snapshot functions and their tests. `rg "PatternFilter"
      mur-core/src mur-agent-runtime/src` — remove `profile.federation.filter` ONLY if
      that search shows no remaining consumer; otherwise leave the profile field and
      file a TODO referencing this plan.
- [x] `cargo nextest run -p mur-core -E 'test(snapshot)'` green; clippy clean:
      `cargo clippy -p mur-core -- -D warnings`.
- [x] Commit: `feat(core): skill snapshot assembly into knowledge_cache`

---

## Task 4 — daemon request processor (mur-daemon)

**Interfaces — Consumes:** `SnapshotRequest` (T1), `SnapshotConfig` (T2),
`assemble_skill_snapshot` (T3). **Produces:** `snapshot_requests::spawn(mur_dir: PathBuf)`.

Steps:

- [x] New file `mur-daemon/src/snapshot_requests.rs`:

```rust
//! Sweep `~/.mur/inbox/snapshot-requests/` for signed SnapshotRequests,
//! verify each against the requesting agent's on-disk pubkey, and assemble
//! its skill snapshot central-side — outside every agent sandbox. This is
//! the enforcement point the spec's trust model requires: nothing from the
//! request payload beyond the (verified) agent name influences assembly.

use std::path::{Path, PathBuf};

use anyhow::Context;
use mur_common::config::Config;
use mur_common::identity::AgentIdentity;
use mur_common::snapshot_request::{SNAPSHOT_REQUEST_DIR, SnapshotRequest};

pub fn spawn(mur_dir: PathBuf) {
    tokio::spawn(async move {
        let cfg = Config::load_or_default(&mur_dir.join("config.yaml"));
        let period = std::time::Duration::from_secs(cfg.federation_snapshot.poll_secs);
        let dir = mur_dir.join(SNAPSHOT_REQUEST_DIR);
        loop {
            if let Err(e) = sweep(&mur_dir, &dir, &cfg) {
                tracing::warn!(error = %e, "snapshot-request sweep failed");
            }
            tokio::time::sleep(period).await;
        }
    });
}

/// Agent names come from request FILES (attacker-writable surface): allow
/// exactly the charset agent dirs use, or the join below is a traversal.
fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn sweep(mur_dir: &Path, dir: &Path, cfg: &Config) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "yaml") {
            continue;
        }
        // Single-shot either way: a rejected request must not be retried
        // forever, and a served one is done. Consume before logging outcome.
        let verdict = process_one(mur_dir, &path, cfg);
        let _ = std::fs::remove_file(&path);
        if let Err(e) = verdict {
            tracing::warn!(request = %path.display(), error = %e, "snapshot request rejected");
        }
    }
    Ok(())
}

fn process_one(mur_dir: &Path, path: &Path, cfg: &Config) -> anyhow::Result<()> {
    let req: SnapshotRequest = serde_yaml_ng::from_str(
        &std::fs::read_to_string(path).context("read request")?,
    )
    .context("parse request")?;
    anyhow::ensure!(valid_agent_name(&req.agent), "invalid agent name");
    anyhow::ensure!(
        req.is_fresh(chrono::Utc::now(), cfg.federation_snapshot.request_max_age_secs),
        "outside freshness window"
    );
    // Trust anchor: the identity in the agent's home. Sandbox write-deny
    // means agent A cannot plant a key under agent B's home, so a key that
    // verifies here belongs to the agent named in the request.
    let identity = AgentIdentity::load(&mur_dir.join("agents").join(&req.agent))
        .context("load agent identity")?;
    anyhow::ensure!(
        req.verify(&identity.verifying_key_bytes()),
        "signature verification failed"
    );
    let snap = mur_core::federation::assemble_skill_snapshot(mur_dir, &req.agent)?;
    tracing::info!(agent = %req.agent, skills = snap.skill_count, "snapshot assembled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// Tempdir MUR home with one agent (real identity) and one Stable skill.
    fn fixture() -> (tempfile::TempDir, AgentIdentity) {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents/t0-agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&agent_dir).unwrap();
        // one Stable global skill (minimal manifest + stats via the canonical path)
        let sdir = home.join("skills/t0-skill");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            sdir.join("skill.yaml"),
            "name: t0-skill\ndescription: tracer\n",
        )
        .unwrap();
        let stats_path = mur_common::skill::stats::SkillStats::path(home, "t0-skill");
        std::fs::create_dir_all(stats_path.parent().unwrap()).unwrap();
        let mut stats = mur_common::skill::stats::SkillStats::default();
        stats.state = mur_common::skill::stats::LifecycleState::Stable;
        std::fs::write(&stats_path, serde_yaml_ng::to_string(&stats).unwrap()).unwrap();
        (tmp, id)
    }

    fn drop_request(home: &Path, req: &SnapshotRequest) -> PathBuf {
        let dir = home.join(SNAPSHOT_REQUEST_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("req.yaml");
        std::fs::write(&p, serde_yaml_ng::to_string(req).unwrap()).unwrap();
        p
    }

    #[test]
    fn valid_request_assembles_cache_and_consumes_file() {
        let (tmp, id) = fixture();
        let home = tmp.path();
        let req = SnapshotRequest::create("t0-agent", &id, Utc::now());
        let p = drop_request(home, &req);
        let cfg = Config::load_or_default(&home.join("config.yaml"));
        sweep(home, &home.join(SNAPSHOT_REQUEST_DIR), &cfg).unwrap();
        assert!(!p.exists(), "request file must be consumed");
        assert!(
            home.join("agents/t0-agent/knowledge_cache/t0-skill/skill.yaml").exists(),
            "stable skill must land in the cache"
        );
    }

    #[test]
    fn bad_signature_is_consumed_without_assembling() {
        let (tmp, _id) = fixture();
        let home = tmp.path();
        // signed by a DIFFERENT key than the one on disk
        let req = SnapshotRequest::create("t0-agent", &AgentIdentity::generate(), Utc::now());
        let p = drop_request(home, &req);
        let cfg = Config::load_or_default(&home.join("config.yaml"));
        sweep(home, &home.join(SNAPSHOT_REQUEST_DIR), &cfg).unwrap();
        assert!(!p.exists());
        assert!(!home.join("agents/t0-agent/knowledge_cache").exists());
    }

    #[test]
    fn traversal_agent_name_is_rejected() {
        let (tmp, id) = fixture();
        let home = tmp.path();
        let mut req = SnapshotRequest::create("t0-agent", &id, Utc::now());
        req.agent = "../t0-agent".into(); // breaks sig too, but the name gate must fire first
        drop_request(home, &req);
        let cfg = Config::load_or_default(&home.join("config.yaml"));
        sweep(home, &home.join(SNAPSHOT_REQUEST_DIR), &cfg).unwrap();
        assert!(!home.join("agents/../t0-agent/knowledge_cache").exists());
    }

    #[test]
    fn stale_request_is_rejected() {
        let (tmp, id) = fixture();
        let home = tmp.path();
        let req = SnapshotRequest::create(
            "t0-agent",
            &id,
            Utc::now() - chrono::Duration::seconds(3600),
        );
        drop_request(home, &req);
        let cfg = Config::load_or_default(&home.join("config.yaml"));
        sweep(home, &home.join(SNAPSHOT_REQUEST_DIR), &cfg).unwrap();
        assert!(!home.join("agents/t0-agent/knowledge_cache").exists());
    }
}
```

- [x] `mur-daemon/src/main.rs`: add `mod snapshot_requests;` and, next to the existing
      `mobile_server::spawn(mur_dir.clone(), …)` call (line ~108),
      `snapshot_requests::spawn(mur_dir.clone());`.
- [x] `cargo nextest run -p mur-daemon -E 'test(snapshot)'` → `4 passed`.
      `cargo clippy -p mur-daemon -- -D warnings` clean.
- [x] Commit: `feat(daemon): verify and serve signed snapshot requests`

---

## Task 5 — runtime drops signed requests (mur-agent-runtime)

**Interfaces — Consumes:** `SnapshotRequest`, `SNAPSHOT_REQUEST_DIR` (T1).
**Produces:** none downstream — this deletes the subprocess path.

Steps:

- [x] Rewrite `refresh_snapshot` in `mur-agent-runtime/src/federation/sync.rs`
      (delete the `std::process::Command` block AND its zombie-reaper thread, sync.rs
      lines ~74-99):

```rust
/// Ask the daemon for a fresh knowledge snapshot by dropping a signed request
/// into `<mur_home>/inbox/snapshot-requests/`. The daemon (outside this
/// sandbox) verifies the signature and assembles the snapshot; this side
/// writes ONE small file and never spawns anything. Same write grant the
/// outbox flush uses (`<mur_home>/inbox/...`).
fn refresh_snapshot(agent_name: &str) {
    if let Err(e) = write_snapshot_request(agent_name) {
        tracing::warn!(agent = %agent_name, error = %e, "snapshot request write failed");
    }
}

fn write_snapshot_request(agent_name: &str) -> anyhow::Result<()> {
    use mur_common::snapshot_request::{SNAPSHOT_REQUEST_DIR, SnapshotRequest};
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    let mur_home = home.join(".mur");
    let identity =
        mur_common::identity::AgentIdentity::load(&mur_home.join("agents").join(agent_name))?;
    let req = SnapshotRequest::create(agent_name, &identity, chrono::Utc::now());
    let dir = mur_home.join(SNAPSHOT_REQUEST_DIR);
    std::fs::create_dir_all(&dir)?;
    // One request per agent pending at a time: deterministic name, tmp+rename.
    let dest = dir.join(format!("{agent_name}.yaml"));
    let tmp = dir.join(format!(".{agent_name}.yaml.tmp"));
    std::fs::write(&tmp, serde_yaml_ng::to_string(&req)?)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(())
}
```

      (`mur_inbox_dir` stays for the outbox flush. If `sync.rs` uses a `MUR_HOME`-aware
      home resolver elsewhere, reuse that instead of raw `dirs::home_dir` — match the
      file's existing resolution, do not introduce a second convention.)
- [x] Test (in `sync.rs` tests, tempdir as home via the same override the file's existing
      tests use — if none exist, take `mur_home: &Path` as a parameter on
      `write_snapshot_request` and have `refresh_snapshot` resolve it, so the test calls
      the parameterized fn):

```rust
    #[test]
    fn snapshot_request_is_written_and_verifies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents/w1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let id = mur_common::identity::AgentIdentity::generate();
        id.save(&agent_dir).unwrap();

        write_snapshot_request_at(home, "w1").unwrap();

        let p = home.join("inbox/snapshot-requests/w1.yaml");
        let req: mur_common::snapshot_request::SnapshotRequest =
            serde_yaml_ng::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert!(req.verify(&id.verifying_key_bytes()));
    }
```

- [x] Grep-guard step: `rg "Command::new" mur-agent-runtime/src/federation/` must return
      nothing.
- [x] `cargo nextest run -p mur-agent-runtime -E 'test(snapshot_request)'` → `1 passed`;
      clippy clean.
- [x] Commit: `feat(runtime): signed snapshot request replaces the mur subprocess pull`

---

## Task 6 — loader reads knowledge_cache (mur-common)

**Interfaces — Consumes:** cache layout from T3. **Produces:** `load_all` returns cached
skills; precedence **agent-local > knowledge_cache > global** via the existing
`seen_names` dedup.

Steps:

- [ ] In `mur-common/src/skill/loader.rs::load_all` (line ~130), between the per-agent
      block and the global block, insert a scan of
      `mur_home/agents/<agent_name>/knowledge_cache/` that loads each
      `<dir>/skill.yaml` through the SAME per-skill load path the other two blocks use
      (`load_one` with the same validity checks), inserting into `seen_names` so
      agent-local wins over cache and cache wins over global. Trust: cached skills load
      with the same `TrustLevel` resolution the global block uses (they ARE the global
      skills, relocated — do not invent a new trust rule here).
- [ ] Tests in `loader.rs`:
  - `knowledge_cache_skill_loads`: tempdir home, skill only in the cache → present in
    `load_all` output.
  - `agent_local_wins_over_cache_wins_over_global`: same name in all three places with
    distinguishable descriptions → the agent-local copy is the one loaded, and with the
    agent-local one removed, the cache copy is.
- [ ] `cargo nextest run -p mur-common -E 'test(knowledge_cache) or test(wins_over)'` →
      `2 passed`.
- [ ] Commit: `feat(common): skill loader reads the per-agent knowledge_cache`

---

## Task 7 — end-to-end smoke test (mur-core)

**Interfaces — Consumes:** everything above. This is P0's exit criterion in test form.

Steps:

- [ ] New test in `mur-core/src/federation/snapshot.rs` tests:

```rust
    #[test]
    fn smoke_assemble_then_loader_sees_the_skill() {
        // P0 exit criterion (spec): one pull → cache → loadable for injection.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        std::fs::create_dir_all(home.join("agents/smoke")).unwrap();
        // Stable global skill (same fixture shape as assemble_ tests)
        write_skill_fixture(home, "smoke-skill", LifecycleState::Stable);

        assemble_skill_snapshot(home, "smoke").unwrap();

        let loaded = mur_common::skill::loader::load_all(home, "smoke");
        assert!(
            loaded.iter().any(|s| s.name == "smoke-skill"),
            "cached skill must be visible to the injection loader"
        );
    }
```

      (`write_skill_fixture` is the helper Task 3's tests already created — reuse it,
      do not write a second fixture.)
- [ ] `cargo nextest run -p mur-core -E 'test(smoke_assemble)'` → `1 passed`.
- [ ] Full gate: `cargo nextest run -p mur-common -p mur-core -p mur-daemon
      -p mur-agent-runtime` green; `cargo clippy --workspace -- -D warnings`;
      `cargo fmt --check`.
- [ ] Commit: `test(core): federation P0 smoke — pull to injectable cache`

---

## Task 8 — T0 tracer bullet (manual checklist, run after P0 merges + binaries installed)

The live-loop acceptance run. Vehicle: a hand-authored tracer skill; agent: `dr_worker_4`
(stopped, expendable — NEVER `dr_worker_1`, it must not be restarted per the standing
keychain constraint). Prereqs: freshly built `mur`, `murmurd`, `mur-agent-runtime`
installed; ad-hoc signing caveat acknowledged (#849).

- [ ] Author the tracer skill (Draft by default):
      `mkdir -p ~/.mur/skills/t0-tracer && $EDITOR ~/.mur/skills/t0-tracer/skill.yaml` —
      minimal manifest: name `t0-tracer`, description "T0 tracer: when asked for the
      tracer phrase, reply exactly TRACER-OK-2026", a matching trigger.
- [ ] Lower the floor for the tracer run ONLY — in `~/.mur/config.yaml`:
      `federation_snapshot:\n  min_lifecycle: draft` (revert at the end).
- [ ] Exercise the review leg: `mur out` → confirm the queue renders (the tracer skill
      is hand-authored so it won't appear here; the leg being exercised is that review
      still works — approve or dismiss any real pending proposals now).
- [ ] Start the daemon in the foreground: `cargo run -p mur-daemon` (or the installed
      `murmurd`) — expect a `snapshot-request` sweep line every ~30s at debug… **expect
      no output**: the runtime filter pins INFO; you will see `snapshot assembled` lines
      only when a request lands.
- [ ] Start the agent from the terminal: `mur agent start dr_worker_4`.
- [ ] Force a sleep-cycle OR wait `agent_idle_minutes`; then verify, in order:
      1. `ls ~/.mur/inbox/snapshot-requests/` — transiently shows `dr_worker_4.yaml`
      2. daemon log line: `snapshot assembled agent=dr_worker_4 skills=N`
      3. `ls ~/.mur/agents/dr_worker_4/knowledge_cache/t0-tracer/` — `skill.yaml` present
      4. `mur agent snapshot show dr_worker_4` — prints the `.snapshot-ref`
- [ ] Behavior change: `mur agent send dr_worker_4 '{"role":"user","parts":[{"kind":"text","text":"Say the tracer phrase."}]}'`
      → reply contains `TRACER-OK-2026`.
- [ ] Negative control: stop the daemon, `rm -rf ~/.mur/agents/dr_worker_4/knowledge_cache`,
      restart the agent, resend → reply does NOT contain the phrase (proves the cache,
      not the prompt, carried it).
- [ ] Clean up: restore `min_lifecycle: stable`, `rm -rf ~/.mur/skills/t0-tracer`,
      stop dr_worker_4.
- [ ] Record outcomes (including any interface friction found) as comments on the spec
      PR — T0 findings feed P1's design, that is its purpose.

## Self-review (before handoff)

- Spec coverage: P0 row → Tasks 1-7; T0 row → Task 8; "no mur spawn" → T5 grep-guard;
  "runtime reads only agent home" → T5 writes only inbox+agent home, reads only agent
  home; smoke exit criterion → T7.
- Cross-task types: `SnapshotRequest` (T1) is the ONLY request type; `SkillSnapshotRef`
  (T3) the only ref type; cache dir name `knowledge_cache` appears in T3/T4/T6/T7/T8
  identically.
- Known thin ice, stated: exact import lists and the `load_one` call signature in T6 come
  from the implementer's compiler, not this plan — shapes and precedence are pinned,
  line-level syntax is not.
