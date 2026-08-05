//! Pull-side propagation sweep (M7c).

pub mod candidates;
pub mod install_ctx;

pub use install_ctx::InstallContext;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use self::candidates::{Candidate, GateConfig, enumerate_candidates};

#[derive(Debug, Clone, Default)]
pub struct PropagateOptions {
    pub gates: GateConfig,
    pub dry_run: bool,
}

#[derive(Debug)]
pub struct PropagateReport {
    pub scanned_peers: usize,
    pub candidates: Vec<Candidate>,
    pub installed: Vec<Candidate>,
    pub failed: Vec<(Candidate, String)>,
}

pub fn run_propagate(
    home: &Path,
    invoking_agent: &str,
    opts: &PropagateOptions,
) -> Result<PropagateReport> {
    let lock_path = lock_path_for_agent(home, invoking_agent);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;
    if lock_file.try_lock_exclusive().is_err() {
        bail!("propagate lock held — another sweep is in progress (exit 7)");
    }

    let peers = mur_common::skill::peers::list_peer_agents(home)?;
    let peers_count = peers.iter().filter(|p| p.name != invoking_agent).count();
    if peers_count == 0 {
        return Ok(PropagateReport {
            scanned_peers: 0,
            candidates: Vec::new(),
            installed: Vec::new(),
            failed: Vec::new(),
        });
    }

    let cands = enumerate_candidates(home, invoking_agent, &opts.gates)?;
    let mut installed: Vec<Candidate> = Vec::new();
    let mut failed: Vec<(Candidate, String)> = Vec::new();

    if !opts.dry_run {
        for cand in &cands {
            let source = format!("agent://{}/{}", cand.source_agent, cand.skill);
            let ctx = InstallContext::AutoPropagate {
                source_fitness: cand.population_fitness,
                source_samples: cand.population_samples,
            };
            let registry_url = "";
            match crate::cmd::skill_install::cmd_install_ctx(
                home,
                registry_url,
                &source,
                invoking_agent,
                ctx,
            ) {
                Ok(()) => installed.push(cand.clone()),
                Err(e) => failed.push((cand.clone(), e.to_string())),
            }
        }
    }

    Ok(PropagateReport {
        scanned_peers: peers_count,
        candidates: cands,
        installed,
        failed,
    })
}

fn lock_path_for_agent(home: &Path, agent: &str) -> PathBuf {
    home.join("agents")
        .join(agent)
        .join("credit")
        .join(".propagate.lock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn lock_blocks_concurrent_sweep() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let lock_path = lock_path_for_agent(home, "alice");
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path)
            .unwrap();
        f.lock_exclusive().unwrap();

        let result = run_propagate(home, "alice", &PropagateOptions::default());
        assert!(
            result.is_err(),
            "second sweep should refuse while lock is held"
        );
        assert!(result.unwrap_err().to_string().contains("exit 7"));
    }

    #[test]
    fn empty_host_yields_empty_report() {
        let d = tempdir().unwrap();
        let home = d.path();
        std::fs::create_dir_all(home.join("agents").join("alice")).unwrap();
        let report = run_propagate(home, "alice", &PropagateOptions::default()).unwrap();
        assert_eq!(report.scanned_peers, 0);
        assert!(report.installed.is_empty());
    }
}
