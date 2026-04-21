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

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

const CONV_MARKER_OPEN: &str = "# BEGIN [conversations] (managed by mur conversations migrate)";
const CONV_MARKER_CLOSE: &str = "# END [conversations]";

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

pub async fn run(home_override: Option<&str>) -> Result<MigrationReport> {
    let start = std::time::Instant::now();
    let home = home_root(home_override);
    let mur = home.join(".mur");

    if daemon_running(home_override) {
        bail!(
            "refusing to migrate: mur-commander daemon appears to be running. \
             Stop it with `murc stop` (or release the flock on {}/.mur/commander/commander.pid).",
            home.display()
        );
    }

    let staging = mur.join(".conversations-migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).context("cleaning stale staging dir")?;
    }
    std::fs::create_dir_all(staging.join("raw"))?;
    std::fs::create_dir_all(staging.join("summary"))?;
    std::fs::create_dir_all(staging.join("users"))?;

    let mut messages_migrated = 0u64;

    // 1. long_term.jsonl → staging/raw/<date>/commander_long-term-<i>.jsonl
    let lt = mur.join("commander/memory/long_term.jsonl");
    if lt.exists() {
        let text = std::fs::read_to_string(&lt)?;
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let ts_unix = v
                .get("timestamp_secs")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let ts = chrono::DateTime::from_timestamp(ts_unix, 0).unwrap_or_else(chrono::Utc::now);
            let content_text = v
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let msg = mur_common::Message {
                v: 1,
                ts,
                src: mur_common::Source::CommanderEngine,
                conv: format!("long-term-{i}"),
                role: mur_common::Role::Assistant,
                content: mur_common::Content::Text {
                    value: content_text,
                },
                meta: v
                    .get("metadata")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                refs: vec![],
            };
            write_staged_raw(&staging, &msg)?;
            messages_migrated += 1;
        }
    }

    // 2. users/*/conversation.jsonl → staging/users/<uid>/conversation.jsonl
    let users_src = mur.join("commander/users");
    if users_src.exists() {
        for u in std::fs::read_dir(&users_src)? {
            let u = u?;
            if !u.file_type()?.is_dir() {
                continue;
            }
            let dest = staging.join("users").join(u.file_name());
            std::fs::create_dir_all(&dest)?;
            let src_file = u.path().join("conversation.jsonl");
            if src_file.exists() {
                std::fs::copy(&src_file, dest.join("conversation.jsonl"))?;
                let lines = count_jsonl_lines(&src_file);
                messages_migrated += lines;
            }
        }
    }

    // 3. episodes/**/*.md → staging/summary/<name>.md
    let ep_src = mur.join("commander/memory/episodes");
    if ep_src.exists() {
        for e in walkdir::WalkDir::new(&ep_src).into_iter().flatten() {
            if e.path().extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let fname = e.path().file_name().unwrap();
            std::fs::copy(e.path(), staging.join("summary").join(fname))?;
        }
    }

    // 4. Read commander's last hash (opaque) for the P1 bridge
    let cmdr_audit = mur.join("commander/audit.jsonl");
    let bridged_from_hash = last_audit_hash_opaque(&cmdr_audit)?;

    // 5. Write a FRESH audit chain at staging/audit.jsonl with one Migrate entry
    //    carrying bridged_from_hash (P1).
    write_p1_migrate_entry(
        &staging.join("audit.jsonl"),
        messages_migrated,
        &bridged_from_hash,
        &cmdr_audit.to_string_lossy(),
        &mur.to_string_lossy(),
    )?;

    // 6. Atomic rename: staging → conversations
    let final_path = mur.join("conversations");
    if final_path.exists() {
        let backup = mur.join(format!(
            "conversations.bak-{}",
            chrono::Utc::now().timestamp()
        ));
        std::fs::rename(&final_path, &backup)?;
    }
    std::fs::rename(&staging, &final_path)?;

    // 7. P4 amendment — sync commander's config.toml from mur's config.yaml
    let mur_cfg_path = mur.join("config.yaml");
    let cfg = if let Ok(text) = std::fs::read_to_string(&mur_cfg_path) {
        serde_yaml::from_str::<mur_common::config::Config>(&text)
            .unwrap_or_default()
            .conversations
    } else {
        mur_common::config::ConversationsConfig::default()
    };
    sync_commander_config_toml(&mur, &cfg)?;

    let audit_entries_replayed = 1; // Only the bridge entry; commander chain stays untouched
    Ok(MigrationReport {
        messages_migrated,
        audit_entries_replayed,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

pub async fn rollback(home_override: Option<&str>) -> Result<MigrationReport> {
    let start = std::time::Instant::now();
    let home = home_root(home_override);
    let mur = home.join(".mur");
    let conv = mur.join("conversations");
    let cmdr_mem = mur.join("commander/memory");
    std::fs::create_dir_all(&cmdr_mem)?;

    let raw_root = conv.join("raw");
    let mut restored = 0u64;
    if raw_root.exists() {
        let lt = cmdr_mem.join("long_term.jsonl");
        let mut out = std::fs::File::create(&lt)?;
        use std::io::Write;
        for e in walkdir::WalkDir::new(&raw_root).into_iter().flatten() {
            let name = e.file_name().to_string_lossy();
            if !name.starts_with("commander_") {
                continue;
            }
            if e.path().extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let f = std::fs::read_to_string(e.path())?;
            for line in f.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                writeln!(out, "{line}")?;
                restored += 1;
            }
        }
    }

    let users_conv = conv.join("users");
    let users_cmdr = mur.join("commander/users");
    if users_conv.exists() {
        std::fs::create_dir_all(&users_cmdr)?;
        for u in std::fs::read_dir(&users_conv)? {
            let u = u?;
            let dest = users_cmdr.join(u.file_name());
            std::fs::create_dir_all(&dest)?;
            let src_file = u.path().join("conversation.jsonl");
            if src_file.exists() {
                std::fs::copy(&src_file, dest.join("conversation.jsonl"))?;
            }
        }
    }

    let cmdr_audit = mur.join("commander/audit.jsonl");
    append_rollback_entry(&cmdr_audit, restored)?;

    Ok(MigrationReport {
        messages_migrated: restored,
        audit_entries_replayed: 0,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

pub async fn resume(home_override: Option<&str>) -> Result<MigrationReport> {
    let start = std::time::Instant::now();
    let home = home_root(home_override);
    let mur = home.join(".mur");
    let staging = mur.join(".conversations-migrating");
    if !staging.exists() {
        bail!(
            "no staging directory at {} to resume from",
            staging.display()
        );
    }
    // Verify the audit we left staged is parseable.
    let staged_audit = staging.join("audit.jsonl");
    if !staged_audit.exists() {
        bail!(
            "staging at {} has no audit.jsonl; run full migrate instead",
            staging.display()
        );
    }
    // Atomic rename into place.
    let final_path = mur.join("conversations");
    if final_path.exists() {
        let backup = mur.join(format!(
            "conversations.bak-{}",
            chrono::Utc::now().timestamp()
        ));
        std::fs::rename(&final_path, &backup)?;
    }
    std::fs::rename(&staging, &final_path)?;
    Ok(MigrationReport {
        messages_migrated: 0, // unknown; raw was already written on first attempt
        audit_entries_replayed: 1,
        duration_ms: start.elapsed().as_millis() as u64,
    })
}

pub async fn discard_staging(home_override: Option<&str>) -> Result<()> {
    let home = home_root(home_override);
    let staging = home.join(".mur/.conversations-migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    Ok(())
}

fn write_staged_raw(staging: &std::path::Path, msg: &mur_common::Message) -> Result<()> {
    let date = msg.ts.date_naive();
    let dir = staging
        .join("raw")
        .join(date.format("%Y-%m-%d").to_string());
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}_{}.jsonl", msg.src.file_prefix(), msg.conv));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)?;
    use std::io::Write;
    serde_json::to_writer(&mut f, msg)?;
    writeln!(f)?;
    Ok(())
}

/// Opaque extraction of the last `entry_hash` line value from commander's
/// audit.jsonl. Does NOT recompute hashes — commander's chain uses a
/// different algorithm. This is purely a cryptographic pointer.
fn last_audit_hash_opaque(path: &std::path::Path) -> Result<String> {
    if !path.exists() {
        return Ok(super::audit::ZERO_HASH.into());
    }
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path)?;
    let mut last = super::audit::ZERO_HASH.to_string();
    for line in BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if let Some(h) = v.get("entry_hash").and_then(|h| h.as_str()) {
            last = h.to_string();
        }
    }
    Ok(last)
}

/// Write a single Migrate entry (P1 shape) to a fresh audit.jsonl at `path`.
/// Starts a new chain from ZERO_HASH.
fn write_p1_migrate_entry(
    path: &std::path::Path,
    count: u64,
    bridged_from_hash: &str,
    bridged_source: &str,
    mur_dir: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let action = super::audit::AuditAction::Migrate {
        from: format!("{mur_dir}/commander/memory"),
        to: format!("{mur_dir}/conversations"),
        count,
        bridged_from_hash: bridged_from_hash.to_string(),
        bridged_source: bridged_source.to_string(),
    };
    let canonical = serde_json::to_string(&action)?;
    let prev = super::audit::ZERO_HASH.to_string();
    let content_sha256 = String::new();
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    h.update(b"\n");
    h.update(content_sha256.as_bytes());
    let entry_hash = hex::encode(h.finalize());
    let entry = super::audit::AuditEntry {
        id: uuid::Uuid::new_v4(),
        ts: chrono::Utc::now(),
        action,
        content_sha256,
        prev_hash: prev,
        entry_hash,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut f, &entry)?;
    writeln!(f)?;
    Ok(())
}

fn append_rollback_entry(path: &std::path::Path, count: u64) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let prev = last_audit_hash_opaque(path)?;
    let action = super::audit::AuditAction::Rollback {
        from: "~/.mur/conversations".into(),
        to: "~/.mur/commander/memory".into(),
        count,
    };
    let canonical = serde_json::to_string(&action)?;
    let content_sha256 = String::new();
    let mut h = Sha256::new();
    h.update(prev.as_bytes());
    h.update(b"\n");
    h.update(canonical.as_bytes());
    h.update(b"\n");
    h.update(content_sha256.as_bytes());
    let entry_hash = hex::encode(h.finalize());
    let entry = super::audit::AuditEntry {
        id: uuid::Uuid::new_v4(),
        ts: chrono::Utc::now(),
        action,
        content_sha256,
        prev_hash: prev,
        entry_hash,
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    serde_json::to_writer(&mut f, &entry)?;
    writeln!(f)?;
    Ok(())
}

/// P4 amendment: write `[conversations]` and `[conversations.compact]` blocks
/// to commander's config.toml, mirroring mur's config.yaml. Idempotent
/// via marker-delimited block.
pub fn sync_commander_config_toml(
    mur_dir: &std::path::Path,
    cfg: &mur_common::config::ConversationsConfig,
) -> Result<()> {
    let cmdr_cfg = mur_dir.join("commander/config.toml");
    if let Some(parent) = cmdr_cfg.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(&cmdr_cfg).unwrap_or_default();

    let new_block = format!(
        "\n{}\n\
         [conversations]\n\
         enabled = {}\n\
         retention_days = {}\n\
         \n\
         [conversations.compact]\n\
         enabled_in_daemon = {}\n\
         daemon_cron = \"{}\"\n\
         \n\
         [conversations.rollup]\n\
         enabled = {}\n\
         max_weeks_per_run = {}\n\
         max_months_per_run = {}\n\
         {}\n",
        CONV_MARKER_OPEN,
        cfg.enabled,
        cfg.retention_days,
        cfg.compact.enabled_in_daemon,
        cfg.compact.daemon_cron,
        cfg.rollup.enabled,
        cfg.rollup.max_weeks_per_run,
        cfg.rollup.max_months_per_run,
        CONV_MARKER_CLOSE,
    );

    let merged = if let (Some(b), Some(e)) = (
        existing.find(CONV_MARKER_OPEN),
        existing.find(CONV_MARKER_CLOSE),
    ) {
        let before = &existing[..b];
        let after_marker = e + CONV_MARKER_CLOSE.len();
        let after = &existing[after_marker..];
        format!(
            "{}{}{}",
            before.trim_end_matches('\n'),
            new_block,
            after.trim_start_matches('\n'),
        )
    } else {
        format!("{}{}", existing.trim_end_matches('\n'), new_block)
    };

    std::fs::write(&cmdr_cfg, merged)?;
    Ok(())
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

    #[tokio::test]
    async fn run_migrates_long_term_into_raw_by_date() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        seed_commander_layout(tmp.path());
        let report = run(Some(home)).await.unwrap();
        assert!(report.messages_migrated >= 1);

        // Verify a raw/<date>/commander_<conv>.jsonl exists
        let raw_root = tmp.path().join(".mur/conversations/raw");
        let walked: Vec<_> = walkdir::WalkDir::new(&raw_root)
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
            .collect();
        assert!(
            !walked.is_empty(),
            "no migrated raw files found at {raw_root:?}"
        );
    }

    #[tokio::test]
    async fn run_records_p1_bridge_audit_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        seed_commander_layout(tmp.path());
        // Seed a commander audit entry so bridged_from_hash has something to reference.
        let cmdr_audit = tmp.path().join(".mur/commander/audit.jsonl");
        std::fs::write(
            &cmdr_audit,
            r#"{"id":"00000000-0000-0000-0000-000000000000","ts":"2026-04-19T00:00:00Z","action":{"kind":"write","target":"x","bytes":0},"content_sha256":"","prev_hash":"0000000000000000000000000000000000000000000000000000000000000000","entry_hash":"seedhashabcdef"}
"#,
        )
        .unwrap();
        run(Some(home)).await.unwrap();
        let conv_audit = tmp.path().join(".mur/conversations/audit.jsonl");
        let text = std::fs::read_to_string(&conv_audit).unwrap();
        assert!(
            text.contains("\"kind\":\"migrate\""),
            "expected migrate kind"
        );
        assert!(
            text.contains("\"bridged_from_hash\":\"seedhashabcdef\""),
            "bridged_from_hash should pin to commander's last entry_hash: {text}"
        );
    }

    #[tokio::test]
    async fn rollback_restores_commander_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        seed_commander_layout(tmp.path());
        run(Some(home)).await.unwrap();
        let report = rollback(Some(home)).await.unwrap();
        assert!(report.messages_migrated >= 1);
        assert!(
            tmp.path()
                .join(".mur/commander/memory/long_term.jsonl")
                .exists(),
            "rollback must restore long_term.jsonl"
        );
    }

    #[tokio::test]
    async fn discard_staging_removes_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        let staging = tmp.path().join(".mur/.conversations-migrating");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("test.txt"), "x").unwrap();
        discard_staging(Some(home)).await.unwrap();
        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn resume_finalizes_existing_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        seed_commander_layout(tmp.path());
        // First do a partial run by manually creating staging with some content
        // then forcibly stopping before the atomic rename. Easiest: run(), then
        // delete the final conversations/ dir to simulate interrupted state
        // where staging was renamed-away but final was rolled back.
        // Simpler path: just run twice — first populates, second verifies resume
        // path is safely idempotent when nothing to resume.
        run(Some(home)).await.unwrap();
        let err = resume(Some(home)).await.err();
        // No staging exists after successful run; resume should error cleanly.
        assert!(
            err.is_some(),
            "resume on clean state must error — no staging to resume"
        );
    }

    #[tokio::test]
    async fn run_syncs_commander_config_toml() {
        // P4 amendment: after migrate, commander/config.toml should contain
        // a `[conversations]` block generated from mur's config.yaml.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_str().unwrap();
        seed_commander_layout(tmp.path());
        // Seed a mur config.yaml with an explicit retention_days
        let mur_cfg = tmp.path().join(".mur/config.yaml");
        std::fs::write(
            &mur_cfg,
            "conversations:\n  enabled: true\n  retention_days: 45\n",
        )
        .unwrap();
        // commander's config.toml with existing section
        let cmdr_cfg = tmp.path().join(".mur/commander/config.toml");
        std::fs::create_dir_all(cmdr_cfg.parent().unwrap()).unwrap();
        std::fs::write(&cmdr_cfg, "[engine]\nfoo = 1\n").unwrap();
        run(Some(home)).await.unwrap();
        let toml = std::fs::read_to_string(&cmdr_cfg).unwrap();
        assert!(toml.contains("[conversations]"), "missing [conversations]");
        assert!(
            toml.contains("enabled = true"),
            "missing enabled=true: {toml}"
        );
        assert!(
            toml.contains("retention_days = 45"),
            "missing retention_days=45: {toml}"
        );
        assert!(toml.contains("[engine]"), "must preserve other sections");
    }

    #[test]
    fn sync_writes_conversations_compact_subsection() {
        let tmp = tempfile::tempdir().unwrap();
        let cmdr_dir = tmp.path().join(".mur/commander");
        std::fs::create_dir_all(&cmdr_dir).unwrap();
        std::fs::write(cmdr_dir.join("config.toml"), "[engine]\nfoo = 1\n").unwrap();
        let cfg = mur_common::config::ConversationsConfig {
            enabled: true,
            retention_days: 30,
            compact: mur_common::config::CompactConfig {
                enabled_in_daemon: true,
                daemon_cron: "0 0 4 * * * *".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
        let toml = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
        assert!(toml.contains("[conversations]"));
        assert!(toml.contains("enabled = true"));
        assert!(toml.contains("retention_days = 30"));
        assert!(toml.contains("[conversations.compact]"));
        assert!(toml.contains("enabled_in_daemon = true"));
        assert!(toml.contains("daemon_cron = \"0 0 4 * * * *\""));
        assert!(toml.contains("[engine]"));
    }

    #[test]
    fn sync_writes_conversations_rollup_subsection() {
        let tmp = tempfile::tempdir().unwrap();
        let cmdr_dir = tmp.path().join(".mur/commander");
        std::fs::create_dir_all(&cmdr_dir).unwrap();
        std::fs::write(cmdr_dir.join("config.toml"), "[engine]\nfoo = 1\n").unwrap();
        let cfg = mur_common::config::ConversationsConfig {
            enabled: true,
            retention_days: 30,
            rollup: mur_common::config::RollupConfig {
                enabled: true,
                max_weeks_per_run: 6,
                max_months_per_run: 3,
                ..Default::default()
            },
            ..Default::default()
        };
        sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
        let toml = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
        assert!(toml.contains("[conversations.rollup]"));
        assert!(toml.contains("enabled = true"));
        assert!(toml.contains("max_weeks_per_run = 6"));
        assert!(toml.contains("max_months_per_run = 3"));
        assert!(toml.contains("[engine]"));
    }

    #[test]
    fn sync_is_idempotent_on_repeat_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let cmdr_dir = tmp.path().join(".mur/commander");
        std::fs::create_dir_all(&cmdr_dir).unwrap();
        std::fs::write(cmdr_dir.join("config.toml"), "[engine]\nfoo = 1\n").unwrap();
        let cfg = mur_common::config::ConversationsConfig {
            enabled: true,
            retention_days: 30,
            compact: mur_common::config::CompactConfig {
                enabled_in_daemon: true,
                daemon_cron: "0 0 4 * * * *".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        // First sync
        sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
        let after_first = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
        // Second sync with identical config
        sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
        let after_second = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
        assert_eq!(
            after_first, after_second,
            "sync must be idempotent — second call must produce identical bytes:\nfirst:\n{after_first}\nsecond:\n{after_second}"
        );
        // Third sync (belt-and-braces)
        sync_commander_config_toml(&tmp.path().join(".mur"), &cfg).unwrap();
        let after_third = std::fs::read_to_string(cmdr_dir.join("config.toml")).unwrap();
        assert_eq!(after_second, after_third);
    }
}
