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
    assert!(stdout.contains(".history/")); // NEW Phase 2C
    assert!(stdout.contains("spans:")); // NEW Phase 3.1
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

#[test]
fn mur_ask_on_empty_archive_returns_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home) = with_mur_home(
        cmd.args(["ask", "What did we build yesterday?"]),
        tmp.path(),
    );
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("don't cover that"),
        "expected fallback text, got: {stdout}"
    );
}

#[test]
fn mur_conversations_preflight_runs_without_ollama() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home) = with_mur_home(cmd.args(["conversations", "preflight"]), tmp.path());
    let out = cmd.output().expect("run mur");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Preflight may exit non-zero when Ollama is unreachable (CI has no Ollama).
    // What we assert: the new probes ran and reported lines for them.
    assert!(
        stdout.contains("Ollama") && stdout.contains("free mem"),
        "expected Phase 2C probes in output. stdout: {stdout}"
    );
}

#[test]
fn mur_conversations_reindex_spans_only_populates_layer_2() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    std::fs::write(
        summary_dir.join("2026-04-21.md"),
        "---\n\
         schema: 1\n\
         date: 2026-04-21\n\
         generated_at: 2026-04-21T03:00:00Z\n\
         generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
         duration_ms: 50\n\
         conv_count: 1\n\
         msg_count: 1\n\
         sources: [cc]\n\
         pattern_refs: []\n\
         keywords: [test]\n\
         links:\n  prev: null\n  next: null\n\
         warnings: []\n\
         input_content_sha: deadbeef\n\
         ---\n\n\
         ## Extractive spans\n\n\
         [1] _{cc/c1 @L1}_:\n> first span\n\n\
         [2] _{cc/c1 @L2}_:\n> second span\n\n\
         ## Abstractive narrative\n\n\
         Narrative.\n",
    )
    .unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _mur_home_val) = with_mur_home(
        cmd.args(["conversations", "reindex", "--spans-only"]),
        tmp.path(),
    );
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("spans"),
        "expected 'spans' in output; got: {stdout}"
    );
}

#[test]
fn mur_conversations_rollup_week_produces_md_and_layer_3_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    // Seed 7 day summaries + layer=2 rows for 2026-W16 (Apr 13..19).
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    for d in 13..=19 {
        std::fs::write(
            summary_dir.join(format!("2026-04-{d:02}.md")),
            format!(
                "---\n\
                 schema: 1\n\
                 date: 2026-04-{d:02}\n\
                 generated_at: 2026-04-{d:02}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {d}sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> day {d} span\n\n\
                 ## Abstractive narrative\n\n\
                 Narrative for day {d}.\n"
            ),
        )
        .unwrap();
    }
    // Need layer=2 spans in the index for rollup to gather. Reindex first
    // with --spans-only.
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex");

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _h) = with_mur_home(
        cmd.args(["conversations", "rollup", "--week", "2026-W16"]),
        tmp.path(),
    );
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let weekly_md = mur_home
        .join("conversations")
        .join("summary")
        .join("weekly")
        .join("2026-W16.md");
    assert!(weekly_md.exists(), "weekly md at {weekly_md:?}");
}

#[test]
fn mur_conversations_doctor_reports_rollup_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
    let (cmd, _h) = with_mur_home(cmd.args(["conversations", "doctor"]), tmp.path());
    let out = cmd.env("MUR_OLLAMA_MOCK", "1").output().expect("run mur");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("weekly rollups"), "got: {stdout}");
    assert!(stdout.contains("monthly rollups"), "got: {stdout}");
}

#[test]
fn mur_ask_continue_appends_to_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // First turn
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "what did I ship this week?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask #1");
    assert!(
        out1.status.success(),
        "first ask stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    let session_path = mur_home.join("conversations").join("ask-session.jsonl");
    assert!(session_path.exists(), "session file missing after turn 1");
    let turn1_lines = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(turn1_lines.lines().count(), 1);

    // Second turn with --continue
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--continue", "what about last week?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask #2");
    assert!(
        out2.status.success(),
        "continue ask stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    let body = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(
        body.lines().count(),
        2,
        "expected 2 turns after --continue, got:\n{body}"
    );

    // Second line should have rewriter_status != "skipped" (since there was a prior turn)
    let last_line = body.lines().last().unwrap();
    let turn2: serde_json::Value = serde_json::from_str(last_line).unwrap();
    let status = turn2["rewriter_status"].as_str().unwrap();
    assert_ne!(
        status, "skipped",
        "turn 2 should have invoked rewriter, got status={status}"
    );
}

#[test]
fn mur_ask_new_archives_prior_session() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    for _ in 0..2 {
        let out = Command::new(env!("CARGO_BIN_EXE_mur"))
            .args(["ask", "first topic question"])
            .env("MUR_HOME", &mur_home)
            .env("HOME", tmp.path())
            .env("USERPROFILE", tmp.path())
            .env("MUR_OLLAMA_MOCK", "1")
            .output()
            .expect("run mur ask");
        assert!(out.status.success());
    }

    // After 2 bare `mur ask` invocations (default-archive-before-each), we
    // expect 1 file in .history/ (the first ask's session was archived when
    // the second ask started fresh).
    let hist = mur_home
        .join("conversations")
        .join("ask-sessions")
        .join(".history");
    let entries: Vec<_> = std::fs::read_dir(&hist)
        .expect("history dir")
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected 1 archived session, got {}",
        entries.len()
    );

    // Active session has 1 turn (the second ask).
    let active = mur_home.join("conversations").join("ask-session.jsonl");
    assert_eq!(std::fs::read_to_string(&active).unwrap().lines().count(), 1);
}

#[test]
fn mur_ask_show_session_prints_summary_without_ollama() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Seed one turn under mock
    let seed = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("seed ask");
    assert!(seed.status.success());

    // --show-session WITHOUT MUR_OLLAMA_MOCK
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--show-session"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        // NOTE: deliberately NOT setting MUR_OLLAMA_MOCK
        .output()
        .expect("run --show-session");
    assert!(
        out.status.success(),
        "show-session stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("turns: 1"), "got:\n{stdout}");
    assert!(stdout.contains("what did I ship?"), "got:\n{stdout}");
    assert!(stdout.contains("session:"), "got:\n{stdout}");
}

/// Phase 3.3 follow-up: streaming-path Error events no longer hard-exit
/// before append_turn. The empty-archive path emits the "don't cover that"
/// fallback Token + Done; ensure that turn still persists to the JSONL.
/// (The pre-fix code path was structurally identical for the
/// generation-Error case, which is harder to exercise without faking Ollama.)
#[test]
fn mur_ask_persists_turn_on_empty_archive_path() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask");
    assert!(
        out.status.success(),
        "mur ask should succeed on empty archive (mock); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let session_path = mur_home.join("conversations").join("ask-session.jsonl");
    assert!(
        session_path.exists(),
        "session file missing after ask on empty archive"
    );
    let body = std::fs::read_to_string(&session_path).unwrap();
    assert_eq!(
        body.lines().count(),
        1,
        "expected 1 turn persisted, got body:\n{body}"
    );
    // The persisted turn should record the empty-archive fallback answer.
    let turn: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
    let answer = turn["answer"].as_str().unwrap_or("");
    assert!(
        answer.contains("don't cover that"),
        "expected 'don't cover that' in answer, got: {answer}"
    );
}

#[test]
fn mur_ask_continue_without_prior_session_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--continue", "follow-up question"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask --continue");
    assert!(
        !out.status.success(),
        "should have exited non-zero on missing prior session"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no prior session"),
        "expected 'no prior session' in stderr, got:\n{stderr}"
    );
}

/// Phase 3.4: with a very tight `max_context_tokens`, `mur ask` should
/// compress hit snippets (Stage 1 of the overflow loop) rather than
/// dropping history / hits. This integration test exercises the end-to-end
/// path: config override → cmd_ask reads config → AskRequest.compress_enabled
/// true → ask_stream passes true → prompt::render Stage 1 fires.
///
/// The test writes a config.yaml with a 500-token budget and asserts that
/// `mur ask --json` returns a successful response (empty-archive fallback
/// answer is sufficient — the key assertion is "process exits clean under
/// tight budget with compression enabled").
#[test]
fn mur_ask_compresses_long_hits_under_tight_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // Config: tight max_context_tokens + compression ON (default).
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 500\n    compress_hits_enabled: true\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("run mur ask --json");
    assert!(
        out.status.success(),
        "mur ask should succeed under tight budget with compression on; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // JSON response should parse and report the empty-archive fallback.
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    let answer = v["answer"].as_str().unwrap_or("");
    assert!(
        answer.contains("don't cover that"),
        "expected empty-archive fallback answer, got: {answer}"
    );
    // Also verify the turn was persisted (compression path doesn't break
    // Phase 3.3 session-JSONL invariant).
    let session = mur_home.join("conversations").join("ask-session.jsonl");
    assert!(session.exists(), "session file missing after ask");
    let body = std::fs::read_to_string(&session).unwrap();
    assert_eq!(
        body.lines().count(),
        1,
        "expected 1 turn persisted, got body:\n{body}"
    );
}

/// Phase 3.2.1: `--if-stale` on `mur conversations rollup --week` must NOT
/// force regeneration. The default behavior (force=false) already triggers
/// the sha-based idempotency check inside rollup_week — running the same
/// rollup twice with --if-stale should produce zero .history/ archive
/// entries (no archive happens when input_content_sha matches existing md).
#[test]
fn mur_conversations_rollup_if_stale_is_idempotent_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Seed 7 day summaries for 2026-W16 (Apr 13-19).
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    for d in 13..=19 {
        std::fs::write(
            summary_dir.join(format!("2026-04-{d:02}.md")),
            format!(
                "---\n\
                 schema: 1\n\
                 date: 2026-04-{d:02}\n\
                 generated_at: 2026-04-{d:02}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {d}sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> day {d} span\n\n\
                 ## Abstractive narrative\n\n\
                 Narrative for day {d}.\n"
            ),
        )
        .unwrap();
    }

    // Populate layer=2 spans via --spans-only reindex (rollup needs them).
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(
        out.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // First rollup invocation.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "conversations",
            "rollup",
            "--week",
            "2026-W16",
            "--if-stale",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first rollup");
    assert!(
        out1.status.success(),
        "first rollup failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Second invocation — same --if-stale flag. Must NOT trigger regeneration.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "conversations",
            "rollup",
            "--week",
            "2026-W16",
            "--if-stale",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second rollup");
    assert!(
        out2.status.success(),
        "second rollup failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Key assertion: the .history/ archive dir is empty (or does not exist).
    // Each regeneration archives the prior md file — if --if-stale forced
    // regen (pre-3.2.1 bug), we'd see 1 archived file. After 3.2.1, zero.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join("weekly")
        .join(".history");
    let archived = if hist.exists() {
        std::fs::read_dir(&hist)
            .unwrap()
            .filter_map(|e| e.ok())
            .count()
    } else {
        0
    };
    assert_eq!(
        archived,
        0,
        "Phase 3.2.1: --if-stale must not force regen. Found {archived} archived files; expected 0. \
         stdout of 2nd call: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// Phase 3.5: with a tight budget + long hits, Stage 1b should fire and JSON
/// should carry `.stage_1b.compressed_count > 0`.
#[test]
fn mur_ask_stage_1b_fires_on_overflow() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // Tight max_context_tokens + summarize_hits_enabled (default true).
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n    compress_hits_enabled: true\n",
    )
    .unwrap();

    // Seed a summary file directly with a long extractive span (>= 400 chars).
    // The mock compact would produce "mock extractive span" (20 chars) which is
    // too short for Stage 1b (MIN_CONTENT_CHARS = 400), so we bypass compact and
    // seed the summary directly.
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    // Long span text: "fact " * 100 = 500 chars (well above MIN_CONTENT_CHARS=400)
    let long_span = "fact ".repeat(100);
    std::fs::write(
        summary_dir.join(format!("{yesterday}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {yesterday}\n\
             generated_at: {yesterday}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.5.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [cc]\n\
             pattern_refs: []\n\
             keywords: [fact]\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: abc123\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{cc/c1 @L1}}_:\n> {long_span}\n\n\
             ## Abstractive narrative\n\n\
             Narrative about facts.\n"
        ),
    )
    .unwrap();

    // Reindex to populate the vector layer so retrieve can surface the span.
    let reindex = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(
        reindex.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex.stderr)
    );

    // Ask with JSON output.
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --json");
    assert!(
        out.status.success(),
        "ask failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse JSON failed: {e}; stdout: {stdout}"));
    let compressed = v
        .pointer("/stage_1b/compressed_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let cache_hits = v
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert!(
        compressed + cache_hits > 0,
        "expected Stage 1b compressed_count+cache_hits > 0 under tight budget; got JSON: {stdout}"
    );
}

/// Phase 3.5: setting `summarize_hits_enabled: false` must short-circuit
/// Stage 1b. JSON must either omit `stage_1b` or have zero counts.
/// The test seeds a long span (500 chars, above MIN_CONTENT_CHARS=400) so that
/// Stage 1b *would* fire if `summarize_hits_enabled` were true — the assertion
/// is only meaningful when there is actually data to compress.
#[test]
fn mur_ask_stage_1b_disabled_via_config() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: false\n    compress_hits_enabled: true\n",
    )
    .unwrap();

    // Seed a long extractive span so Stage 1b *would* fire if enabled.
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    let long_span = "fact ".repeat(100); // 500 chars, above MIN_CONTENT_CHARS=400
    std::fs::write(
        summary_dir.join(format!("{yesterday}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {yesterday}\n\
             generated_at: {yesterday}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.5.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [cc]\n\
             pattern_refs: []\n\
             keywords: [fact]\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: abc789\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{cc/c1 @L1}}_:\n> {long_span}\n\n\
             ## Abstractive narrative\n\n\
             Narrative about facts.\n"
        ),
    )
    .unwrap();

    // Reindex to populate the vector layer.
    let reindex_out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex");
    assert!(
        reindex_out.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex_out.stderr)
    );

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what did I ship?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --json");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    let stage_1b_present_and_nonzero = v.get("stage_1b").is_some_and(|s| {
        s.get("compressed_count")
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
            > 0
    });
    assert!(
        !stage_1b_present_and_nonzero,
        "Stage 1b must not fire when disabled; got: {stdout}"
    );
}

/// Phase 3.5: second ask over the same seeded archive and question should
/// see `.stage_1b.cache_hits > 0` (fewer fresh compressions, more cache
/// hits) when the first ask's inputs warm the cache.
#[test]
fn mur_ask_stage_1b_cache_hits_on_second_run() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    // Seed summary directly with a long span (>= 400 chars) so Stage 1b fires.
    // Mock compact produces "mock extractive span" (20 chars) which is too short.
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    let long_span = "fact ".repeat(100); // 500 chars, above MIN_CONTENT_CHARS=400
    std::fs::write(
        summary_dir.join(format!("{yesterday}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {yesterday}\n\
             generated_at: {yesterday}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.5.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [cc]\n\
             pattern_refs: []\n\
             keywords: [fact]\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: abc123\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{cc/c1 @L1}}_:\n> {long_span}\n\n\
             ## Abstractive narrative\n\n\
             Narrative about facts.\n"
        ),
    )
    .unwrap();

    let reindex_out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex");
    assert!(
        reindex_out.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex_out.stderr)
    );

    // First ask — starts new session, warms the abstractive cache.
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 1");

    // Second ask — identical question → same cache key. Cache is keyed on
    // model + target + content, not on session state.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 2");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout2).expect("parse JSON");
    let cache_hits = v
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert!(
        cache_hits > 0,
        "second ask should see cache_hits > 0; got JSON: {stdout2}"
    );
}

/// Phase 3.5: when Stage 1b hits a timeout, the ask must still succeed
/// (soft-fail). The answer is produced from the original un-summarized
/// hits; exit code is zero.
#[test]
fn mur_ask_stage_1b_soft_fails_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    // Seed summary directly with a long span (>= 400 chars) so Stage 1b actually
    // attempts LLM compression, which then hits the MUR_ABSTRACTIVE_MOCK_FAIL=timeout.
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    let long_span = "fact ".repeat(100); // 500 chars, above MIN_CONTENT_CHARS=400
    std::fs::write(
        summary_dir.join(format!("{yesterday}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {yesterday}\n\
             generated_at: {yesterday}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.5.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [cc]\n\
             pattern_refs: []\n\
             keywords: [fact]\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: abc456\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{cc/c1 @L1}}_:\n> {long_span}\n\n\
             ## Abstractive narrative\n\n\
             Narrative about facts.\n"
        ),
    )
    .unwrap();

    let reindex_out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex");
    assert!(
        reindex_out.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex_out.stderr)
    );

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .env("MUR_ABSTRACTIVE_MOCK_FAIL", "timeout")
        .output()
        .expect("ask with FAIL=timeout");
    assert!(
        out.status.success(),
        "soft-fail: ask must still exit 0 when Stage 1b times out; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("parse JSON");
    let skipped = v
        .pointer("/stage_1b/skipped_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert!(
        skipped > 0,
        "timeout must register as skipped_count > 0; got: {stdout}"
    );
}

/// Phase 3.5.1: `--no-summarize` flag must disable Stage 1b for the
/// invocation regardless of the config. JSON must omit `stage_1b` or
/// report `compressed_count + cache_hits == 0`. Mirrors
/// `mur_ask_stage_1b_disabled_via_config` but disables via CLI rather than
/// writing config.
#[test]
fn mur_ask_cli_no_summarize_flag_disables_stage_1b() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    // Config has Stage 1b ENABLED — the CLI flag must override.
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    seed_rich_span(&mur_home, "2026-04-21", "cc/c1", "sha-no-summarize");

    let reindex = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(
        reindex.status.success(),
        "reindex failed: {}",
        String::from_utf8_lossy(&reindex.stderr)
    );

    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "--no-summarize", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("mur ask --no-summarize");
    assert!(
        out.status.success(),
        "ask failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("parse JSON failed: {e}; stdout: {stdout}"));
    let compressed = v
        .pointer("/stage_1b/compressed_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let cache_hits = v
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert_eq!(
        compressed + cache_hits,
        0,
        "--no-summarize must prevent Stage 1b from firing; got: {stdout}"
    );
}

/// Phase 3.5.1: `--summarize-model <X>` must produce a different cache key
/// than the default model, so a query that warmed the cache under the
/// default model must see `cache_hits == 0` when re-run with a different
/// `--summarize-model`. The third run with the SAME --summarize-model value
/// should see `cache_hits > 0`, confirming the new key is stable.
#[test]
fn mur_ask_cli_summarize_model_changes_cache_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    std::fs::create_dir_all(&mur_home).unwrap();
    std::fs::write(
        mur_home.join("config.yaml"),
        "conversations:\n  ask:\n    max_context_tokens: 400\n    summarize_hits_enabled: true\n",
    )
    .unwrap();

    seed_rich_span(&mur_home, "2026-04-21", "cc/c1", "sha-model-cachekey");

    let reindex = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");
    assert!(reindex.status.success());

    // Run 1 — default model (config has none, so falls back to ask.model =
    // "qwen3:4b"). Warms the cache under that key.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["ask", "--json", "what was discussed?"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 1");
    assert!(out1.status.success());

    // Run 2 — override model to qwen3:9b. Cache key is different → must
    // fresh-compress, cache_hits == 0.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "ask",
            "--json",
            "--summarize-model",
            "qwen3:9b",
            "what was discussed?",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 2");
    assert!(out2.status.success());
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    let v2: serde_json::Value = serde_json::from_str(&stdout2).expect("parse JSON run 2");
    let cache_hits_2 = v2
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    let compressed_2 = v2
        .pointer("/stage_1b/compressed_count")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert_eq!(
        cache_hits_2, 0,
        "run 2 (different model) must NOT hit the run-1 cache key; got: {stdout2}"
    );
    assert!(
        compressed_2 > 0,
        "run 2 must fresh-compress under the new key; got: {stdout2}"
    );

    // Run 3 — same --summarize-model qwen3:9b. Must now hit run 2's cache.
    let out3 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args([
            "ask",
            "--json",
            "--summarize-model",
            "qwen3:9b",
            "what was discussed?",
        ])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("ask 3");
    assert!(out3.status.success());
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    let v3: serde_json::Value = serde_json::from_str(&stdout3).expect("parse JSON run 3");
    let cache_hits_3 = v3
        .pointer("/stage_1b/cache_hits")
        .and_then(|n| n.as_u64())
        .unwrap_or(0);
    assert!(
        cache_hits_3 > 0,
        "run 3 (same model as run 2) must hit run 2's cache; got: {stdout3}"
    );
}

/// Phase 3.5.1 — shared seeding helper. Writes a single summary markdown
/// under `<mur_home>/conversations/summary/` with an extractive span long
/// enough (~500 chars) to qualify for Stage 1b (`MIN_CONTENT_CHARS = 400`)
/// and with no `. ` terminators so Stage 1's heuristic compression skips
/// it (`COMPRESS_MIN_SENTENCES = 4`).
fn seed_rich_span(mur_home: &std::path::Path, date: &str, conv_ref: &str, sha: &str) {
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    let span_text = "fact ".repeat(100); // 500 chars, zero ". " terminators
    let (src_prefix, _conv_id) = conv_ref.split_once('/').unwrap();
    std::fs::write(
        summary_dir.join(format!("{date}.md")),
        format!(
            "---\n\
             schema: 1\n\
             date: {date}\n\
             generated_at: {date}T03:00:00Z\n\
             generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
             duration_ms: 50\n\
             conv_count: 1\n\
             msg_count: 1\n\
             sources: [{src_prefix}]\n\
             pattern_refs: []\n\
             keywords: []\n\
             links:\n  prev: null\n  next: null\n\
             warnings: []\n\
             input_content_sha: {sha}\n\
             ---\n\n\
             ## Extractive spans\n\n\
             [1] _{{{conv_ref} @L1}}_:\n> {span_text}\n\n\
             ## Abstractive narrative\n\n\
             Narrative.\n"
        ),
    )
    .unwrap();
}

/// Phase 3.2.1: --force MUST still regenerate unconditionally, even when the
/// content is fresh. This test verifies we didn't break --force while
/// fixing --if-stale.
#[test]
fn mur_conversations_rollup_force_still_regenerates() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");

    // Same 7-day seed as above.
    let summary_dir = mur_home.join("conversations").join("summary");
    std::fs::create_dir_all(&summary_dir).unwrap();
    for d in 13..=19 {
        std::fs::write(
            summary_dir.join(format!("2026-04-{d:02}.md")),
            format!(
                "---\n\
                 schema: 1\n\
                 date: 2026-04-{d:02}\n\
                 generated_at: 2026-04-{d:02}T03:00:00Z\n\
                 generated_by:\n  extractive_model: qwen3:14b\n  abstractive_model: qwen3:14b\n  mur_version: 3.0.0\n\
                 duration_ms: 50\n\
                 conv_count: 1\n\
                 msg_count: 1\n\
                 sources: [cc]\n\
                 pattern_refs: []\n\
                 keywords: []\n\
                 links:\n  prev: null\n  next: null\n\
                 warnings: []\n\
                 input_content_sha: {d}sha\n\
                 ---\n\n\
                 ## Extractive spans\n\n\
                 [1] _{{cc/c1 @L1}}_:\n> day {d} span\n\n\
                 ## Abstractive narrative\n\n\
                 Narrative for day {d}.\n"
            ),
        )
        .unwrap();
    }
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "reindex", "--spans-only"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("reindex --spans-only");

    // First rollup.
    let _ = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first rollup");

    // Second rollup with --force — must archive the prior md.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "rollup", "--week", "2026-W16", "--force"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second rollup --force");
    assert!(
        out2.status.success(),
        "second rollup --force failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Verify --force triggered a .history/ entry.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join("weekly")
        .join(".history");
    assert!(
        hist.exists(),
        ".history/ must exist after --force triggered an archive"
    );
    let archived = std::fs::read_dir(&hist)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert!(
        archived >= 1,
        "Phase 3.2.1: --force must still regenerate. Found {archived} archived files; expected ≥1. \
         stdout of --force call: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// Windows CI Hardening Phase 1 — adversarial regression guard for the
/// "same-wall-clock-second byte-equality swallows --force" bug class.
///
/// Phase 3.5 fixed this for `write_rollup` after it flaked on Windows;
/// Phase 1 of the hardening effort fixes the matching shape in
/// `write_summary` and locks the invariant with this test. Fails if any
/// future writer reintroduces the byte-equality noop short-circuit without
/// a `!force` guard.
///
/// Pairs with `mur_conversations_rollup_force_still_regenerates` (Phase 3.2.1).
#[test]
fn mur_conversations_compact_force_unconditionally_archives() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join(".mur");
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // Seed one raw JSONL line so compact has something to do.
    let raw = mur_home.join("conversations").join("raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "c1",
        "role": "user",
        "content": {"t": "text", "v": "seed content for force-archive test"},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_c1.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();

    // First compact — produces .md under summary/<date>.md.
    let out1 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("first compact");
    assert!(
        out1.status.success(),
        "first compact failed: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Immediately re-compact with --force. Same wall-clock second is
    // possible on a fast runner. Pre-fix, the byte-equality short-circuit
    // in `write_summary` swallows --force silently. Post-fix, the !force
    // guard archives the prior md unconditionally.
    let out2 = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact", "--force"])
        .env("MUR_HOME", &mur_home)
        .env("HOME", tmp.path())
        .env("USERPROFILE", tmp.path())
        .env("MUR_OLLAMA_MOCK", "1")
        .output()
        .expect("second compact --force");
    assert!(
        out2.status.success(),
        "second compact --force failed: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    // Assertion: .history/ exists with ≥1 archived entry.
    let hist = mur_home
        .join("conversations")
        .join("summary")
        .join(".history");
    assert!(
        hist.exists(),
        ".history/ must exist after --force triggered an archive; \
         stdout of --force call:\n{}",
        String::from_utf8_lossy(&out2.stdout)
    );
    let archived = std::fs::read_dir(&hist)
        .unwrap()
        .filter_map(|e| e.ok())
        .count();
    assert!(
        archived >= 1,
        "Phase 1 hardening: compact --force must unconditionally archive \
         the prior md even when the body is byte-identical. Found \
         {archived} archived files; expected ≥1. stdout:\n{}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

// ── I5 — ask::generate::stream_answer end-to-end against a wiremocked
// Anthropic SSE response. Hits the full path: factory builds a real
// AnthropicBackend (wrapped in RetryingBackend), stream_answer wires a
// ChatRequest, the SSE parser yields ChatChunk items, and the adapter
// surfaces them to the caller as text deltas. Closes I5.
//
// Note: stream_answer's contract is `Stream<Item = Result<String>>` —
// it intentionally drops the trailing usage chunk (token accounting is
// estimated downstream from the forwarded text). Usage propagation
// through the SSE parser itself is verified at the lower layer by I3
// (`factory_retries_anthropic_503_then_streams_via_real_sse_parser`).
#[tokio::test]
async fn ask_generate_against_wiremocked_anthropic_sse_streams_text() {
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ENV_LOCK is internal to mur-core; skip locking in the integration
    // crate. We use a unique env-var name so this test doesn't collide
    // with other integration tests if they ever land.
    unsafe { std::env::remove_var("MUR_LLM_MOCK") };
    unsafe { std::env::remove_var("MUR_OLLAMA_MOCK") };
    unsafe { std::env::set_var("MUR_TEST_ANTHROPIC_KEY_I5", "k") };

    let server = MockServer::start().await;
    let sse = "event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n\
        event: content_block_delta\n\
        data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n\
        event: message_delta\n\
        data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}\n\n\
        event: message_stop\n\
        data: {\"type\":\"message_stop\"}\n\n";
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse),
        )
        .mount(&server)
        .await;

    let cfg = mur_common::config::BackendConfig {
        provider: "anthropic".into(),
        model: "claude-haiku-4-5".into(),
        endpoint: Some(server.uri()),
        api_key_env: Some("MUR_TEST_ANTHROPIC_KEY_I5".into()),
        api_key_ref: None,
        timeout_secs: Some(5),
    };
    let backend = mur_core::conversations::backend::factory::build(&cfg).unwrap();
    let mut stream = mur_core::conversations::ask::generate::stream_answer(
        backend.as_ref(),
        "claude-haiku-4-5",
        "you are a tester",
        "say hello world",
        16,
    )
    .await
    .unwrap();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        text.push_str(&chunk.unwrap());
    }
    assert_eq!(text, "hello world");
    unsafe { std::env::remove_var("MUR_TEST_ANTHROPIC_KEY_I5") };
}
