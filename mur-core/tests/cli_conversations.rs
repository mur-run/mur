use std::process::Command;

#[test]
fn mur_chat_list_runs_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["chat", "list"])
        .env("HOME", tmp.path())
        .output()
        .expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn mur_conversations_doctor_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "doctor"])
        .env("HOME", tmp.path())
        .output()
        .expect("run mur");
    assert!(out.status.success());
}
