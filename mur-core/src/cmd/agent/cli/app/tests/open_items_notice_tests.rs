//! `open_items_notice_tests`, one module per file so no file passes CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented by one level, nothing else.

use super::super::*;

fn app_with_muted(home: &tempfile::TempDir, muted: &str) -> App {
    std::fs::write(
        home.path().join("config.yaml"),
        format!("open_items:\n  muted:\n    - {muted}\n"),
    )
    .unwrap();
    let session = Session::create(home.path(), "a").unwrap();
    App::new(
        home.path().to_path_buf(),
        "a".into(),
        session,
        &super::super::super::theme::ANSI,
    )
}

fn add_proposals(home: &tempfile::TempDir, names: &[&str]) {
    let dir = home.path().join("inbox").join("workflow-proposals");
    std::fs::create_dir_all(&dir).unwrap();
    for n in names {
        std::fs::write(dir.join(format!("{n}.yaml")), "name: x\n").unwrap();
    }
}

/// The turn notice is where noise costs most, because it interrupts. A
/// muted source must not wake it — not on the first turn, and not when it
/// churns.
#[test]
fn a_muted_source_never_wakes_the_turn_notice() {
    let home = tempfile::tempdir().unwrap();
    add_proposals(&home, &["a", "b", "c"]);
    let mut app = app_with_muted(&home, "inbox");

    let before = app.messages.len();
    app.note_open_items_if_changed();
    assert_eq!(app.messages.len(), before, "muted source produced a line");

    // The muted source changes. Still nothing.
    add_proposals(&home, &["d", "e"]);
    app.note_open_items_if_changed();
    assert_eq!(app.messages.len(), before, "muted churn produced a line");
}

/// ...and the mute must not silence everything else with it, or the notice
/// is dead rather than quiet.
#[test]
fn an_unmuted_source_still_speaks() {
    let home = tempfile::tempdir().unwrap();
    add_proposals(&home, &["a"]);
    let mut app = app_with_muted(&home, "something-else");

    let before = app.messages.len();
    app.note_open_items_if_changed();
    assert_eq!(app.messages.len(), before + 1);
    assert!(
        app.messages.last().unwrap().text.contains("open item"),
        "{:?}",
        app.messages.last()
    );
}
