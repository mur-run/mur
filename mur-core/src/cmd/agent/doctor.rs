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
    /// The agent's `profile.yaml` / `sys_prompt.md` was edited after this
    /// process started, so its live entitlements are NOT what the files say.
    ///
    /// Distinct from `stale`, which compares the runtime BINARY's git sha:
    /// nothing was watching for a config edit. `mur agent perm …` warns once,
    /// at edit time, and then the discrepancy is invisible — which is how a
    /// grant that "was definitely added" silently fails to apply.
    pub profile_drift: bool,
}

/// Build a doctor row for a single agent from its parsed lock file.
///
/// Pure function — no I/O — so it is cheap to unit-test.
pub fn build_row(name: &str, lock: &LockFile, disk_sha: &str, profile_drift: bool) -> AgentRow {
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
        profile_drift,
    }
}

/// Probe `/Volumes` for the macOS Full-Disk-Access EPERM (`os error 1`) that
/// blocks access to removable/external volumes, and return the shared guidance
/// string when — and only when — that exact failure is observed (issue #1).
///
/// Returns `None` when `/Volumes` is readable, absent, or fails for any other
/// reason (we must not hijack unrelated errors). Split out as a pure-ish
/// function taking the probe closure so it is cheap to unit-test without
/// touching the real filesystem.
pub fn removable_volume_hint<F>(probe: F) -> Option<&'static str>
where
    F: Fn(&std::path::Path) -> std::io::Result<()>,
{
    let volumes = std::path::Path::new("/Volumes");
    match probe(volumes) {
        Err(e) if e.raw_os_error() == Some(1) => Some(mur_common::REMOVABLE_VOLUME_EPERM_HINT),
        _ => None,
    }
}

/// Real filesystem probe: attempt to enumerate `/Volumes`. A metadata/read
/// attempt that trips macOS EPERM surfaces `os error 1`.
fn probe_volumes(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::read_dir(path).map(|_| ())
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
            let drift = mur_common::agent_facts::agent_facts(&mur_home, &agent_name)
                .is_some_and(|f| f.drift);
            rows.push(build_row(&agent_name, &lock, &disk_sha, drift));
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
                    "profile_drift": r.profile_drift,
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
            } else if r.profile_drift {
                println!(
                    "{}: running, binary current but PROFILE EDITED SINCE START \u{2192} run 'mur agent restart {}'",
                    r.name, r.name
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

    // ── Removable-volume Full-Disk-Access preflight (issue #1) ──────────────
    // Independent of the stale check: if `/Volumes` is blocked by macOS EPERM,
    // emit the shared guidance. Printed to stderr so it never pollutes the
    // `--json` stdout payload.
    if let Some(hint) = removable_volume_hint(probe_volumes) {
        eprintln!("\n{hint}");
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
        let row = build_row("myagent", &lock, "999999999999", false);
        assert!(row.stale);
        assert_eq!(row.name, "myagent");
        assert_eq!(row.lock_sha, "abc123def456");
        assert_eq!(row.disk_sha, "999999999999");
    }

    #[test]
    fn build_row_current_when_shas_match() {
        let lock = make_lock("abc123def456");
        let row = build_row("myagent", &lock, "abc123def456", false);
        assert!(!row.stale);
    }

    #[test]
    fn profile_drift_is_independent_of_binary_staleness() {
        // The two conditions are orthogonal: a current binary can still be
        // running yesterday's entitlements, which is the case nothing used to
        // report.
        let lock = make_lock("abc123def456");
        let row = build_row("myagent", &lock, "abc123def456", true);
        assert!(!row.stale);
        assert!(row.profile_drift);
    }

    #[test]
    fn build_row_empty_lock_sha_shown_as_unknown() {
        let lock = make_lock("");
        let row = build_row("myagent", &lock, "abc123def456", false);
        assert!(row.stale);
        assert_eq!(row.lock_sha, "unknown");
    }

    #[test]
    fn build_row_two_unknowns_not_stale() {
        let lock = make_lock("unknown");
        let row = build_row("myagent", &lock, "unknown", false);
        assert!(!row.stale);
    }

    #[test]
    fn removable_hint_shown_on_eperm() {
        // Probe reports EPERM (os error 1) against /Volumes → guidance shown,
        // and it is the exact shared mur-common string (no drift).
        let hint = removable_volume_hint(|_p| Err(std::io::Error::from_raw_os_error(1)));
        assert_eq!(hint, Some(mur_common::REMOVABLE_VOLUME_EPERM_HINT));
    }

    #[test]
    fn removable_hint_absent_when_readable() {
        // /Volumes readable → no guidance.
        let hint = removable_volume_hint(|_p| Ok(()));
        assert!(hint.is_none());
    }

    #[test]
    fn removable_hint_ignores_non_eperm_errors() {
        // ENOENT (no /Volumes at all) or any non-EPERM error must NOT trigger
        // the removable-volume guidance.
        let enoent = removable_volume_hint(|_p| Err(std::io::Error::from_raw_os_error(2)));
        assert!(enoent.is_none());
        let eacces = removable_volume_hint(|_p| Err(std::io::Error::from_raw_os_error(13)));
        assert!(eacces.is_none());
    }
}
