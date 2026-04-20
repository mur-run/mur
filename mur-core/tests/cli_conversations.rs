use std::path::{Path, PathBuf};
use std::process::Command;

/// Pin mur's data root to `<tmp>/.mur` via `MUR_HOME`.
///
/// Also sets `HOME` / `USERPROFILE` to `tmp` for any code that reaches for
/// `dirs::home_dir()` outside the conversations paths module (e.g. tracing
/// setup, legacy callers). On Windows, only `MUR_HOME` is authoritative
/// because `dirs` calls `SHGetKnownFolderPath` and bypasses env entirely.
fn with_mur_home<'a>(cmd: &'a mut Command, tmp: &Path) -> (&'a mut Command, PathBuf) {
    let mur_home = tmp.join(".mur");
    cmd.env("MUR_HOME", &mur_home)
        .env("HOME", tmp)
        .env("USERPROFILE", tmp);
    (cmd, mur_home)
}

#[test]
fn mur_chat_list_runs_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home) = with_mur_home(cmd.args(["chat", "list"]), tmp.path());
    let out = cmd.output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn mur_conversations_doctor_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home) = with_mur_home(cmd.args(["conversations", "doctor"]), tmp.path());
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("raw day-dirs"));
    assert!(stdout.contains("summaries:")); // NEW Phase 2A
    assert!(stdout.contains("Ollama")); // NEW Phase 2A
}

#[test]
fn mur_conversations_compact_on_empty_archive_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home) = with_mur_home(cmd.args(["conversations", "compact"]), tmp.path());
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(nothing to compact)") || stdout.contains("done:"),
        "unexpected output: {stdout}"
    );
}

#[test]
fn mur_conversations_compact_on_seeded_day_produces_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "c1",
        "role": "user",
        "content": {"t": "text", "v": "mock extractive span seeded for compact test"},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home_arg) = with_mur_home(cmd.args(["conversations", "compact"]), tmp.path());
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
    let summary = mur_home
        .join("conversations")
        .join("summary")
        .join(format!("{yesterday}.md"));
    assert!(
        summary.exists(),
        "summary should have been written at {summary:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
    let body = std::fs::read_to_string(&summary).unwrap();
    assert!(body.contains("## Extractive spans"));
    assert!(body.contains("## Abstractive narrative"));
}
