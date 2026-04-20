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

#[test]
fn mur_conversations_compact_on_empty_archive_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("HOME", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur");
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
    let home = tmp.path();
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let raw = home
        .join(".mur")
        .join("conversations")
        .join("raw")
        .join(&yesterday);
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
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("HOME", home)
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let summary = home
        .join(".mur")
        .join("conversations")
        .join("summary")
        .join(format!("{yesterday}.md"));
    assert!(
        summary.exists(),
        "summary should have been written at {summary:?}"
    );
    let body = std::fs::read_to_string(&summary).unwrap();
    assert!(body.contains("## Extractive spans"));
    assert!(body.contains("## Abstractive narrative"));
}
