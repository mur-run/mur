use super::*;
use mur_compress::{AutoCfg, CompressConfig, CompressEngine};
use serde_json::json;

/// Mirrors the `engine()` test helper in `mur-compress/src/auto.rs`: a
/// throwaway store dir plus a default-config `CompressEngine`.
fn engine() -> (tempfile::TempDir, CompressEngine) {
    let dir = tempfile::tempdir().unwrap();
    let eng = CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap();
    (dir, eng)
}

fn auto_cfg() -> AutoCfg {
    AutoCfg {
        enabled: true,
        claude_hook: true,
        ..AutoCfg::default()
    }
}

/// Large, repetitive JSON array well above the `min_tokens` gate — stands
/// in for an oversized MCP tool result.
fn big_tool_response() -> serde_json::Value {
    let items: Vec<String> = (0..4000)
        .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\",\"value\":{}}}", i * 7))
        .collect();
    serde_json::from_str(&format!("[{}]", items.join(","))).unwrap()
}

#[test]
fn oversized_mcp_tool_result_compresses_and_prints_line() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let raw = json!({
        "tool_name": "mcp__desktop-commander__list_processes",
        "tool_response": big_tool_response(),
    });

    let line = compress_tool_response(&raw, &cfg, &eng).expect("gate should fire");
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("line must be valid JSON");

    assert_eq!(
        parsed["hookSpecificOutput"]["hookEventName"],
        json!("PostToolUse")
    );
    let updated = &parsed["hookSpecificOutput"]["updatedToolOutput"];
    // An array response has no tool-declared shape to preserve (this path
    // serves MCP list results), so it keeps the object envelope. What must
    // NOT happen is the envelope arriving JSON-stringified — Claude Code
    // validates against the originating tool's schema and discards it.
    assert!(
        updated.is_object(),
        "envelope must not be stringified; got: {updated}"
    );
    assert_eq!(updated["compressed"], json!(true));
    assert!(
        updated["hash"].as_str().is_some_and(|h| !h.is_empty()),
        "offloaded result must carry a hash: {updated}"
    );
    assert!(
        updated["note"]
            .as_str()
            .is_some_and(|n| n.contains("mur_retrieve")),
        "expected retrieval marker in note: {updated}"
    );
}

#[test]
fn object_tool_response_keeps_object_shape() {
    // Claude Code validates `updatedToolOutput` against the originating
    // tool's output schema. Edit/Write/Bash/Agent all hand the hook an
    // object, so a stringified replacement is rejected with "expected
    // object, received string" and the compression is silently dropped.
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let raw = json!({
        "tool_name": "Bash",
        "tool_response": {
            "stdout": big_tool_response().to_string(),
            "stderr": "",
            "interrupted": false,
        },
    });

    let line = compress_tool_response(&raw, &cfg, &eng).expect("gate should fire");
    let parsed: serde_json::Value = serde_json::from_str(&line).expect("line must be valid JSON");
    let updated = &parsed["hookSpecificOutput"]["updatedToolOutput"];

    assert!(
        updated.is_object(),
        "must stay an object or Claude Code discards it; got: {updated}"
    );
    assert_eq!(updated["interrupted"], json!(false), "siblings preserved");
    assert_eq!(updated["stderr"], json!(""), "siblings preserved");
    // `stdout` was declared a string by the tool, so the compressed form
    // must still be a string — the retrieval hash is inlined as text
    // rather than swapped in as the object envelope.
    let stdout = updated["stdout"]
        .as_str()
        .expect("stdout must stay a string, not become the object envelope");
    assert!(
        stdout.contains("mur_retrieve"),
        "compressed stdout should carry the retrieval marker: {stdout}"
    );
}

#[test]
fn small_tool_response_below_floor_is_none() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let raw = json!({
        "tool_name": "mcp__desktop-commander__list_processes",
        "tool_response": "tiny output",
    });

    assert!(compress_tool_response(&raw, &cfg, &eng).is_none());
}

#[test]
fn already_compressed_envelope_is_none() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let raw = json!({
        "tool_name": "mcp__desktop-commander__list_processes",
        "tool_response": {
            "compressed": true,
            "content": "already compressed",
            "hash": "deadbeef",
            "original_tokens": 9000,
            "compressed_tokens": 12,
            "note": "Large output compressed; original stored.",
        },
    });

    assert!(compress_tool_response(&raw, &cfg, &eng).is_none());
}

#[test]
fn mur_own_compress_tools_are_exempt() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let big: Vec<_> = (0..2000)
        .map(|i| json!({"idx": i, "data": "x".repeat(40)}))
        .collect();
    for name in [
        "mcp__mur__mur_retrieve",
        "mcp__mur__mur_compress",
        "mcp__mur__mur_compress_stats",
    ] {
        let raw = json!({"tool_name": name, "tool_response": big});
        assert!(
            compress_tool_response(&raw, &cfg, &eng).is_none(),
            "{name} must never be re-compressed"
        );
    }
}

/// Without the mur MCP server there is no `mur_retrieve` tool, so a stub
/// can only be read back by shelling out. Compressing that output rebuilds
/// the stub and the entry is unreadable for the rest of the session.
#[test]
fn cli_retrieve_through_bash_is_exempt_too() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let big: Vec<_> = (0..2000)
        .map(|i| json!({"idx": i, "data": "x".repeat(40)}))
        .collect();
    for command in [
        "mur retrieve abc123",
        "mur compress --file big.txt",
        "./target/debug/mur retrieve abc123",
        "/usr/local/bin/mur retrieve abc123 | tail -40",
        "MUR_HOME=/tmp/x mur retrieve abc123",
    ] {
        let raw = json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": big,
        });
        assert!(
            compress_tool_response(&raw, &cfg, &eng).is_none(),
            "must not re-compress: {command}"
        );
    }
}

#[test]
fn ordinary_bash_output_is_still_compressed() {
    let (_dir, eng) = engine();
    let cfg = auto_cfg();
    let big: Vec<_> = (0..2000)
        .map(|i| json!({"idx": i, "data": "x".repeat(40)}))
        .collect();
    // Neither a bare `mur` subcommand nor an unrelated command may buy
    // exemption — the guard is loose, not open.
    for command in ["mur model list", "cat big.json", "murmur"] {
        let raw = json!({
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": big.clone(),
        });
        assert!(
            compress_tool_response(&raw, &cfg, &eng).is_some(),
            "should still compress: {command}"
        );
    }
}
