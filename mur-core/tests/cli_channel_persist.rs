use mur_core::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

#[test]
fn cli_turns_persist_to_channel_and_resume() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let sess = Session::create(home, "qa").unwrap();
    sess.append("user", "find the bug", None).unwrap();
    sess.append("agent", "found it", Some("t-1")).unwrap();
    let cid = sess.channel_id().to_string();
    drop(sess);

    let latest = persist::latest(home, "qa").unwrap().expect("a session");
    assert_eq!(latest.id, cid);
    let turns = persist::load(home, &latest.id, "qa").unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "agent");
    assert_eq!(turns[1].text, "found it");
}
