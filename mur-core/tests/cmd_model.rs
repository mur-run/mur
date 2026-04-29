//! Integration tests for `mur model` subcommands.

use std::process::Command;

fn mur_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mur")
}

fn run_with_home(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(mur_bin())
        .env("HOME", home)
        .env_remove("MUR_HOME")
        .args(args)
        .output()
        .expect("spawn mur")
}

#[test]
fn add_list_show_remove_round_trip() {
    let dir = tempfile::tempdir().unwrap();

    let add = run_with_home(
        dir.path(),
        &[
            "model",
            "add",
            "x",
            "--provider",
            "ollama",
            "--model",
            "llama3:3b",
        ],
    );
    assert!(
        add.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let list = run_with_home(dir.path(), &["model", "list"]);
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(stdout.contains("x\tollama\tllama3:3b"), "list: {stdout}");

    let show = run_with_home(dir.path(), &["model", "show", "x"]);
    let show_out = String::from_utf8_lossy(&show.stdout);
    assert!(show_out.contains("provider: ollama"), "show: {show_out}");

    let rm = run_with_home(dir.path(), &["model", "remove", "x"]);
    assert!(rm.status.success(), "remove failed");

    let list2 = run_with_home(dir.path(), &["model", "list"]);
    assert!(
        String::from_utf8_lossy(&list2.stdout).contains("(no models registered)"),
        "list2: {}",
        String::from_utf8_lossy(&list2.stdout)
    );
}

#[test]
fn show_missing_errors() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_with_home(dir.path(), &["model", "show", "nope"]);
    assert!(!out.status.success(), "expected non-zero exit");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not found"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn add_with_invalid_secret_errors() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_with_home(
        dir.path(),
        &[
            "model",
            "add",
            "bad",
            "--provider",
            "ollama",
            "--model",
            "m",
            "--secret",
            "bogus:x",
        ],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unknown scheme")
            || String::from_utf8_lossy(&out.stderr).contains("invalid SecretRef"),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
