//! Commander → conversations migration (spec §7).
//!
//! Phase 1 ships:
//! - `dry_run(home)` — scan commander paths, count data, estimate space.
//! - `daemon_running(home)` — P3 amendment: real flock on commander.pid.
//!
//! Task 19 adds:
//! - `run(home)` — staged atomic migration.
//! - `rollback(home)` — restore commander layout.
//! - `resume(home)` + staging recovery.

use anyhow::{Result, bail};
use std::path::PathBuf;

#[derive(Debug)]
pub struct MigrationPlan {
    pub long_term_lines: u64,
    pub user_turns: u64,
    pub user_count: u64,
    pub episode_count: u64,
    pub audit_entries: u64,
    pub current_usage_bytes: u64,
    pub free_space_needed_bytes: u64,
    pub commander_daemon_running: bool,
}

#[derive(Debug)]
pub struct MigrationReport {
    pub messages_migrated: u64,
    pub audit_entries_replayed: u64,
    pub duration_ms: u64,
}

fn home_root(home_override: Option<&str>) -> PathBuf {
    home_override
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap())
}

fn count_jsonl_lines(p: &std::path::Path) -> u64 {
    if !p.exists() {
        return 0;
    }
    let Ok(content) = std::fs::read_to_string(p) else {
        return 0;
    };
    content.lines().filter(|l| !l.trim().is_empty()).count() as u64
}

pub fn dry_run(home_override: Option<&str>) -> Result<MigrationPlan> {
    let home = home_root(home_override);
    let mur = home.join(".mur");

    let lt = mur.join("commander/memory/long_term.jsonl");
    let long_term_lines = count_jsonl_lines(&lt);

    let users_dir = mur.join("commander/users");
    let mut user_turns = 0u64;
    let mut user_count = 0u64;
    if users_dir.exists() {
        for u in std::fs::read_dir(&users_dir)? {
            let u = u?;
            if !u.file_type()?.is_dir() {
                continue;
            }
            user_count += 1;
            user_turns += count_jsonl_lines(&u.path().join("conversation.jsonl"));
        }
    }

    let episodes_dir = mur.join("commander/memory/episodes");
    let mut episode_count = 0u64;
    if episodes_dir.exists() {
        for e in walkdir::WalkDir::new(&episodes_dir).into_iter().flatten() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("md") {
                episode_count += 1;
            }
        }
    }

    let audit_entries = count_jsonl_lines(&mur.join("commander/audit.jsonl"));
    let current = dir_size_bytes(&mur.join("commander/memory")).unwrap_or(0);
    let free_space_needed_bytes = current + current / 2; // 1.5x safety

    Ok(MigrationPlan {
        long_term_lines,
        user_turns,
        user_count,
        episode_count,
        audit_entries,
        current_usage_bytes: current,
        free_space_needed_bytes,
        commander_daemon_running: daemon_running(home_override),
    })
}

fn dir_size_bytes(p: &std::path::Path) -> Result<u64> {
    if !p.exists() {
        return Ok(0);
    }
    let mut t = 0u64;
    for e in std::fs::read_dir(p)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            t += dir_size_bytes(&e.path())?;
        } else {
            t += e.metadata()?.len();
        }
    }
    Ok(t)
}

/// P3 amendment: detect a running commander daemon by attempting an exclusive
/// flock on `~/.mur/commander/commander.pid`. File existence alone is
/// unreliable — stale PID files persist after crashes.
pub fn daemon_running(home_override: Option<&str>) -> bool {
    use fs2::FileExt;
    let home = home_root(home_override);
    let pid_path = home.join(".mur/commander/commander.pid");
    if !pid_path.exists() {
        return false;
    }
    let Ok(f) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pid_path)
    else {
        return false;
    };
    match f.try_lock_exclusive() {
        Ok(()) => {
            let _ = FileExt::unlock(&f);
            false // We got the lock -> nobody else holds it.
        }
        Err(_) => true, // Lock held elsewhere -> daemon running.
    }
}

pub fn render_plan(p: &MigrationPlan) -> String {
    format!(
        "Migration plan (commander → conversations):\n  \
         long_term.jsonl: {} lines\n  \
         users: {} with {} turns total\n  \
         episodes: {} md files\n  \
         audit: {} entries\n  \
         current usage: {:.1} MB\n  \
         free space needed: {:.1} MB (1.5x safety)\n  \
         commander daemon running: {}\n",
        p.long_term_lines,
        p.user_count,
        p.user_turns,
        p.episode_count,
        p.audit_entries,
        p.current_usage_bytes as f64 / 1_048_576.0,
        p.free_space_needed_bytes as f64 / 1_048_576.0,
        p.commander_daemon_running,
    )
}

pub async fn run(_home_override: Option<&str>) -> Result<MigrationReport> {
    bail!("migrate run not yet implemented — see Task 19")
}

pub async fn rollback(_home_override: Option<&str>) -> Result<MigrationReport> {
    bail!("rollback not yet implemented — see Task 19")
}

pub async fn resume(_home_override: Option<&str>) -> Result<MigrationReport> {
    bail!("resume not yet implemented — see Task 19")
}

pub async fn discard_staging(_home_override: Option<&str>) -> Result<()> {
    bail!("discard_staging not yet implemented — see Task 19")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_commander_layout(home: &std::path::Path) {
        let mem = home.join(".mur/commander/memory");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("long_term.jsonl"),
            r#"{"id":"1","text":"hi","metadata":{},"timestamp_secs":1776571759,"vector":[]}
"#,
        )
        .unwrap();
        let u = home.join(".mur/commander/users/alice");
        std::fs::create_dir_all(&u).unwrap();
        std::fs::write(
            u.join("conversation.jsonl"),
            r#"{"timestamp":1776571759,"role":"user","text":"hello"}
"#,
        )
        .unwrap();
    }

    #[test]
    fn dry_run_counts_everything() {
        let tmp = tempfile::tempdir().unwrap();
        seed_commander_layout(tmp.path());
        let home = tmp.path().to_str().unwrap();
        let plan = dry_run(Some(home)).unwrap();
        assert_eq!(plan.long_term_lines, 1);
        assert_eq!(plan.user_turns, 1);
        assert_eq!(plan.user_count, 1);
        assert!(plan.free_space_needed_bytes > 0);
    }

    #[test]
    fn dry_run_on_clean_install_has_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".mur")).unwrap();
        let plan = dry_run(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(plan.long_term_lines, 0);
        assert_eq!(plan.user_turns, 0);
    }

    #[test]
    fn daemon_running_reports_false_when_pid_absent() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".mur/commander")).unwrap();
        assert!(!daemon_running(Some(tmp.path().to_str().unwrap())));
    }

    #[test]
    fn daemon_running_detects_held_flock() {
        // P3 amendment — real flock check, not file-existence only.
        use fs2::FileExt;
        let tmp = tempfile::tempdir().unwrap();
        let cmdr = tmp.path().join(".mur/commander");
        std::fs::create_dir_all(&cmdr).unwrap();
        let pid_path = cmdr.join("commander.pid");
        std::fs::write(&pid_path, "12345").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&pid_path)
            .unwrap();
        held.try_lock_exclusive().unwrap();
        assert!(
            daemon_running(Some(tmp.path().to_str().unwrap())),
            "expected daemon_running=true when PID file is flocked"
        );
        FileExt::unlock(&held).unwrap();
        assert!(
            !daemon_running(Some(tmp.path().to_str().unwrap())),
            "expected daemon_running=false once flock released"
        );
    }
}
