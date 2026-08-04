//! Sweep `~/.mur/inbox/snapshot-requests/` for signed SnapshotRequests,
//! verify each against the requesting agent's on-disk pubkey, and assemble
//! its skill snapshot central-side — outside every agent sandbox. This is
//! the enforcement point the federation spec's trust model requires:
//! nothing from the request payload beyond the (verified) agent name
//! influences what gets assembled.

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

/// Agent names arrive from request FILES (an attacker-writable surface):
/// allow exactly the charset agent dirs use, or the path join below is a
/// traversal primitive.
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
        // forever, and a served one is done. Consume, then log the outcome.
        let verdict = process_one(mur_dir, &path, cfg);
        let _ = std::fs::remove_file(&path);
        if let Err(e) = verdict {
            tracing::warn!(request = %path.display(), error = %e, "snapshot request rejected");
        }
    }
    Ok(())
}

fn process_one(mur_dir: &Path, path: &Path, cfg: &Config) -> anyhow::Result<()> {
    let req: SnapshotRequest =
        serde_yaml_ng::from_str(&std::fs::read_to_string(path).context("read request")?)
            .context("parse request")?;
    anyhow::ensure!(valid_agent_name(&req.agent), "invalid agent name");
    anyhow::ensure!(
        req.is_fresh(
            chrono::Utc::now(),
            cfg.federation_snapshot.request_max_age_secs
        ),
        "outside freshness window"
    );
    // Trust anchor: the identity in the agent's home. Sandbox write-deny
    // means agent A cannot plant a key under agent B's home, so a key that
    // verifies here belongs to the agent named in the request.
    let identity = AgentIdentity::load(&mur_dir.join("agents").join(&req.agent))
        .map_err(|e| anyhow::anyhow!("load agent identity: {e}"))?;
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
    use mur_common::skill::stats::{LifecycleState, SkillStats};

    /// Tempdir MUR home with one agent (real identity) and one Stable skill.
    fn fixture() -> (tempfile::TempDir, AgentIdentity) {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents/t0-agent");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&agent_dir).unwrap();
        // one Stable global skill (manifest + stats.json at the canonical path)
        let sdir = home.join("skills/t0-skill");
        std::fs::create_dir_all(&sdir).unwrap();
        std::fs::write(
            sdir.join("skill.yaml"),
            "name: t0-skill\ndescription: tracer\n",
        )
        .unwrap();
        let mut stats = SkillStats::new("t0-skill", "1.0.0", "digest", Utc::now());
        stats.lifecycle_state = LifecycleState::Stable;
        std::fs::write(
            SkillStats::path(home, "t0-skill"),
            serde_json::to_string(&stats).unwrap(),
        )
        .unwrap();
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
            home.join("agents/t0-agent/knowledge_cache/t0-skill/skill.yaml")
                .exists(),
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
        req.agent = "../t0-agent".into(); // breaks the sig too; the name gate must fire first
        drop_request(home, &req);
        let cfg = Config::load_or_default(&home.join("config.yaml"));
        sweep(home, &home.join(SNAPSHOT_REQUEST_DIR), &cfg).unwrap();
        assert!(!home.join("agents/../t0-agent/knowledge_cache").exists());
        assert!(!home.join("agents/t0-agent/knowledge_cache").exists());
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
