use mur_core::cmd::agent::cli::persist::{self, Session};
use tempfile::TempDir;

#[test]
fn cli_turns_persist_to_channel_and_resume() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let mut sess = Session::create(home, "qa").unwrap();
    sess.append("user", "find the bug", None, &[]).unwrap();
    sess.append("agent", "found it", Some("t-1"), &[]).unwrap();
    let cid = sess
        .channel_id()
        .expect("channel created on append")
        .to_string();
    drop(sess);

    let latest = persist::latest(home, "qa").unwrap().expect("a session");
    assert_eq!(latest.id, cid);
    let turns = persist::load(home, &latest.id, "qa").unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[1].role, "agent");
    assert_eq!(turns[1].text, "found it");
}

#[test]
fn empty_session_creates_no_channel_and_resume_skips_it() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    // Launch a session and type nothing (lazy create => no channel on disk).
    let sess = Session::create(home, "qa").unwrap();
    assert!(sess.channel_id().is_none());
    drop(sess);
    assert!(persist::latest(home, "qa").unwrap().is_none());

    // A real conversation, then an empty session afterwards (e.g. /clear).
    let mut real = Session::create(home, "qa").unwrap();
    real.append("user", "hello", None, &[]).unwrap();
    let real_id = real.channel_id().unwrap().to_string();
    drop(real);
    let _empty = Session::create(home, "qa").unwrap(); // never appended
    drop(_empty);

    // --resume resolves to the real conversation, never the blank one.
    let latest = persist::latest(home, "qa")
        .unwrap()
        .expect("the real session");
    assert_eq!(latest.id, real_id);
    assert_eq!(latest.turns, 1);
}
