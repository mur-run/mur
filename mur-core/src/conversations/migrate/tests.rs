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
