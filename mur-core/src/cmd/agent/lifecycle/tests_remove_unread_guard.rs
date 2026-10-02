use super::*;
use tempfile::TempDir;

fn write_inbox_unread(dir: &Path, id: &str) {
    let inbox = dir.join("companion/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let body = format!(
        "---\nid: {id}\nsituation: morning_greeting\ntemplate_id: t\nlocale: en-US\ngenerated_at: 2026-04-29T08:00:00+00:00\n---\n\nHello!\n\n>>> response: <unset>\n"
    );
    std::fs::write(inbox.join(format!("{id}.md")), body).unwrap();
}

fn write_inbox_acked(dir: &Path, id: &str) {
    let inbox = dir.join("companion/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let body = format!(
        "---\nid: {id}\nsituation: morning_greeting\ntemplate_id: t\nlocale: en-US\ngenerated_at: 2026-04-29T08:00:00+00:00\n---\n\nHello!\n\n>>> response: good\n"
    );
    std::fs::write(inbox.join(format!("{id}.md")), body).unwrap();
}

#[test]
fn count_unread_returns_zero_for_no_inbox() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(count_unread_companion_inbox(tmp.path()), 0);
}

#[test]
fn count_unread_counts_only_unset_response() {
    let tmp = TempDir::new().unwrap();
    write_inbox_unread(tmp.path(), "msg-001");
    write_inbox_unread(tmp.path(), "msg-002");
    write_inbox_acked(tmp.path(), "msg-003");
    assert_eq!(count_unread_companion_inbox(tmp.path()), 2);
}
