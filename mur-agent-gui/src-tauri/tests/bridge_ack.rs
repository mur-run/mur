//! companion_ack writes the runtime CLI's signal back into the inbox file.

use mur_agent_gui_lib::companion_bridge::commands::companion_ack_inner;
use tempfile::TempDir;

#[test]
fn ack_good_rewrites_response_line() {
    let dir = TempDir::new().unwrap();
    let inbox = dir.path().join("agents/alex/companion/inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let path = inbox.join("01HACK_001.md");
    std::fs::write(
        &path,
        "\
---
id: 01HACK_001
situation: morning_greeting
template_id: t
locale: en
generated_at: 2026-05-02T07:00:00Z
---

hi

>>> response: <unset>",
    )
    .unwrap();

    companion_ack_inner(dir.path(), "alex", "01HACK_001", "good").unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.ends_with(">>> response: good"), "got: {after}");
}

#[test]
fn ack_unknown_id_errors() {
    let dir = TempDir::new().unwrap();
    let err = companion_ack_inner(dir.path(), "alex", "ghost", "good").unwrap_err();
    assert!(format!("{err:#}").contains("ghost"));
}

#[test]
fn ack_invalid_signal_errors() {
    let dir = TempDir::new().unwrap();
    let err = companion_ack_inner(dir.path(), "alex", "x", "shrug").unwrap_err();
    assert!(format!("{err:#}").contains("signal"));
}
