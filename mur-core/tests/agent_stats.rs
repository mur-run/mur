// Windows: gated — depends on `mur agent create` (unix symlink) +
// telemetry JSONL written by an `mur agent` runtime invocation.
#![cfg(unix)]

use std::process::Command;
use tempfile::TempDir;

fn mur_create(mur_home: &std::path::Path, bin_dir: &std::path::Path, name: &str) {
    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home)
        .env("MUR_AGENT_BIN_DIR", bin_dir)
        .env("MUR_AGENT_RUNTIME_BIN", "/tmp/runtime-stub")
        .args(["agent", "create", name, "--no-interactive"])
        .output()
        .expect("spawn mur create");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_telemetry(mur_home: &std::path::Path, name: &str, lines: &[&str]) {
    let dir = mur_home.join("agents").join(name).join("telemetry");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("2026-04-23.jsonl"), lines.join("\n") + "\n").unwrap();
}

#[test]
fn stats_aggregates_telemetry_events() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    write_telemetry(
        mur_home.path(),
        "agent_x",
        &[
            r#"{"gen_ai.provider.name":"ollama","gen_ai.request.model":"m","gen_ai.usage.input_tokens":10,"gen_ai.usage.output_tokens":5,"latency_ms":100}"#,
            r#"{"gen_ai.provider.name":"ollama","gen_ai.request.model":"m","gen_ai.usage.input_tokens":20,"gen_ai.usage.output_tokens":7,"latency_ms":200}"#,
            r#"{"kind":"llm_rate_limit","message":"429","recoverable":true}"#,
        ],
    );

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "stats", "agent_x"])
        .output()
        .expect("spawn mur stats");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    // Two LLM call rows had input_tokens 10+20 and output_tokens 5+7.
    assert!(
        body.contains("llm_calls: 2"),
        "missing llm_calls count: {body}"
    );
    assert!(
        body.contains("input_tokens: 30"),
        "missing input total: {body}"
    );
    assert!(
        body.contains("output_tokens: 12"),
        "missing output total: {body}"
    );
    assert!(
        body.contains("avg_latency_ms: 150"),
        "missing avg latency: {body}"
    );
    assert!(body.contains("errors: 1"), "missing errors count: {body}");
}

#[test]
fn logs_tail_prints_last_n_stderr_lines() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");
    let log_path = mur_home.path().join("agents/agent_x/stderr.log");
    std::fs::write(
        &log_path,
        "line1\nline2\nline3\nline4\nline5\nline6\nline7\n",
    )
    .unwrap();

    let mur = env!("CARGO_BIN_EXE_mur");
    let out = Command::new(mur)
        .env("MUR_HOME", mur_home.path())
        .args(["agent", "logs", "agent_x", "--tail", "3"])
        .output()
        .expect("spawn mur logs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = String::from_utf8(out.stdout).unwrap();
    assert!(body.contains("line5"));
    assert!(body.contains("line6"));
    assert!(body.contains("line7"));
    assert!(!body.contains("line4"), "should not include line4: {body}");
}
