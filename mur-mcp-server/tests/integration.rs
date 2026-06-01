// mur-mcp-server/tests/integration.rs
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

fn send_request(stdin: &mut impl Write, request: &str) {
    writeln!(stdin, "{}", request).unwrap();
}

fn read_response(stdout: &mut impl BufRead) -> serde_json::Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn test_initialize_and_list_tools() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    // Initialize
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    );
    let resp = read_response(&mut stdout);
    assert_eq!(resp["id"], 1);
    assert!(resp["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap()
        .contains("mur"));

    // Confirm initialization
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    // Consume the notification acknowledgment
    let _ = read_response(&mut stdout);

    // List tools
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    let resp = read_response(&mut stdout);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6, "Expected 6 tools");

    // Verify tool names
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mur_notes_search"));
    assert!(names.contains(&"mur_notes_show"));
    assert!(names.contains(&"mur_project_search"));
    assert!(names.contains(&"mur_project_status"));
    assert!(names.contains(&"mur_agent_status"));
    assert!(names.contains(&"mur_hook_context"));

    child.kill().ok();
}

#[test]
fn test_tools_list_response_under_token_budget() {
    // Verify tools/list JSON stays under ~5000 tokens (~25,000 chars).
    let mut child = Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());

    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    );
    let _ = read_response(&mut stdout);
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    let _ = read_response(&mut stdout);
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );

    let resp = read_response(&mut stdout);
    let tools_json = serde_json::to_string(&resp["result"]["tools"]).unwrap();
    // 6 tools should be well under 25,000 chars (5,000 token budget)
    assert!(
        tools_json.len() < 25_000,
        "tools/list response is {} chars, must stay under 25,000 (5,000 token budget)",
        tools_json.len()
    );

    child.kill().ok();
}
