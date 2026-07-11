//! `mur agent runtime-doctor` — flag running agents whose binary differs from
//! the on-disk runtime (build-sha compare).

use anyhow::{Context as _, Result};
use mur_common::LockFile;

use super::{resolve_mur_home, stale};

/// One row in the doctor report.
pub struct AgentRow {
    pub name: String,
    pub lock_sha: String,
    pub disk_sha: String,
    pub stale: bool,
}

/// Build a doctor row for a single agent from its parsed lock file.
///
/// Pure function — no I/O — so it is cheap to unit-test.
pub fn build_row(name: &str, lock: &LockFile, disk_sha: &str) -> AgentRow {
    let lock_sha = if lock.build_sha.is_empty() {
        "unknown".to_string()
    } else {
        lock.build_sha.clone()
    };
    AgentRow {
        name: name.to_string(),
        lock_sha,
        disk_sha: disk_sha.to_string(),
        stale: stale::is_stale(lock, disk_sha),
    }
}

/// `mur agent runtime-doctor [--json]`
///
/// Enumerates all agents that have a `running.lock`, compares each one's
/// recorded `build_sha` against the on-disk runtime binary, and reports
/// which ones are stale.  Exits non-zero when at least one agent is stale.
pub fn cmd_doctor(json: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agents_dir = mur_home.join("agents");

    // Compute the on-disk sha ONCE — avoids redundant subprocess calls.
    let disk_sha = stale::on_disk_sha();

    let mut rows: Vec<AgentRow> = Vec::new();

    if agents_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&agents_dir)
            .with_context(|| format!("read {}", agents_dir.display()))?
            .filter_map(|e| e.ok())
            .collect();
        // Stable, deterministic output order.
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let agent_name = entry.file_name().to_string_lossy().to_string();
            let lock_path = entry.path().join("running.lock");
            if !lock_path.exists() {
                continue; // agent not running
            }
            let bytes = match std::fs::read(&lock_path) {
                Ok(b) => b,
                Err(_) => continue, // unreadable — skip
            };
            let lock: LockFile = match serde_json::from_slice(&bytes) {
                Ok(l) => l,
                Err(_) => continue, // malformed lock — skip
            };
            rows.push(build_row(&agent_name, &lock, &disk_sha));
        }
    }

    // ── Output ──────────────────────────────────────────────────────────────
    let any_stale = rows.iter().any(|r| r.stale);

    if json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name":     r.name,
                    "running":  true,
                    "stale":    r.stale,
                    "lock_sha": r.lock_sha,
                    "disk_sha": r.disk_sha,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr)?);
    } else {
        if rows.is_empty() {
            println!("No running agents found.");
        }
        for r in &rows {
            if r.stale {
                let lock8 = r.lock_sha.chars().take(8).collect::<String>();
                let disk8 = r.disk_sha.chars().take(8).collect::<String>();
                println!(
                    "{}: running, STALE (lock {} vs disk {}) \u{2192} run 'mur agent restart {}'",
                    r.name, lock8, disk8, r.name
                );
            } else {
                println!("{}: running, current", r.name);
            }

            // Best-effort program-deps preflight — never blocks or fails the
            // doctor. A load/aggregate error is swallowed; the runtime-doctor
            // report is unrelated to whether this agent's declared programs
            // are present, so it must never gate on it.
            let _ = (|| -> Result<()> {
                let deps = crate::cmd::deps::aggregate_agent(&mur_home, &r.name)?;
                let report = crate::cmd::deps::doctor::build_report(&deps, &mur_home);
                crate::cmd::deps::doctor::print_report(
                    &report,
                    &format!("mur agent install-deps {}", r.name),
                );
                Ok(())
            })();
        }
    }

    if any_stale {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::LockFile;

    fn make_lock(build_sha: &str) -> LockFile {
        LockFile {
            schema: 1,
            uuid: "test-uuid".to_string(),
            name: "test-agent".to_string(),
            pid: 12345,
            ppid: 1,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            binary_version: "1.0.0".to_string(),
            transports: mur_common::agent::LockTransports {
                stdio: true,
                unix_socket: None,
                tcp: None,
                webhook: None,
            },
            card_digest: "abc".to_string(),
            capabilities: vec![],
            build_sha: build_sha.to_string(),
            proto_version: 1,
        }
    }

    #[test]
    fn build_row_stale_when_shas_differ() {
        let lock = make_lock("abc123def456");
        let row = build_row("myagent", &lock, "999999999999");
        assert!(row.stale);
        assert_eq!(row.name, "myagent");
        assert_eq!(row.lock_sha, "abc123def456");
        assert_eq!(row.disk_sha, "999999999999");
    }

    #[test]
    fn build_row_current_when_shas_match() {
        let lock = make_lock("abc123def456");
        let row = build_row("myagent", &lock, "abc123def456");
        assert!(!row.stale);
    }

    #[test]
    fn build_row_empty_lock_sha_shown_as_unknown() {
        let lock = make_lock("");
        let row = build_row("myagent", &lock, "abc123def456");
        assert!(row.stale);
        assert_eq!(row.lock_sha, "unknown");
    }

    #[test]
    fn build_row_two_unknowns_not_stale() {
        let lock = make_lock("unknown");
        let row = build_row("myagent", &lock, "unknown");
        assert!(!row.stale);
    }
}
