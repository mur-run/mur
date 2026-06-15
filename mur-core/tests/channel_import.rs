use std::fs;
use tempfile::TempDir;

#[test]
fn imports_cli_session_into_channel() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let sess_dir = home.join("agents/qa/cli-sessions");
    fs::create_dir_all(&sess_dir).unwrap();
    let jsonl = "\
{\"ts\":\"2026-06-15T10:00:00+00:00\",\"role\":\"user\",\"text\":\"hi\"}\n\
{\"ts\":\"2026-06-15T10:00:01+00:00\",\"role\":\"agent\",\"text\":\"hello\",\"task_id\":\"t-1\"}\n";
    fs::write(sess_dir.join("018f00000000-aaaa.jsonl"), jsonl).unwrap();

    let n = mur_core::cmd::agent::channel_import::migrate_all(home).unwrap();
    assert_eq!(n, 1);

    let svc = mur_channel::ChannelService::open(home).unwrap();
    let rows = svc.list(10).unwrap();
    assert_eq!(rows.len(), 1);
    let evs = svc.load_events(&rows[0].id).unwrap();
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[1].payload["text"], "hello");

    // Re-running is idempotent (marker file).
    let n2 = mur_core::cmd::agent::channel_import::migrate_all(home).unwrap();
    assert_eq!(n2, 0);
}
