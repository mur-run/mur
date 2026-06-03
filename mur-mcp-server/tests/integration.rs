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
    assert!(
        resp["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap()
            .contains("mur")
    );

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
    assert_eq!(tools.len(), 10, "Expected 10 tools");

    // Verify properties is an object (not an array) — MCP spec requires JSON object
    let first_tool_with_props = tools
        .iter()
        .find(|t| !t["inputSchema"]["properties"].is_null())
        .unwrap();
    let props = first_tool_with_props["inputSchema"]["properties"].as_object();
    assert!(
        props.is_some(),
        "inputSchema.properties must be a JSON object (record), not an array"
    );

    // Verify tool names
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"mur_notes_search"));
    assert!(names.contains(&"mur_notes_show"));
    assert!(names.contains(&"mur_project_search"));
    assert!(names.contains(&"mur_project_status"));
    assert!(names.contains(&"mur_agent_status"));
    assert!(names.contains(&"mur_hook_context"));
    assert!(names.contains(&"vlc_open"));
    assert!(names.contains(&"vlc_playback"));
    assert!(names.contains(&"vlc_status"));
    assert!(names.contains(&"scene_explain"));

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

#[test]
fn lists_project_search_tool() {
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

    // Confirm initialization (required by MCP protocol)
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

    let names: Vec<String> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "mur_project_search"),
        "tools/list must include mur_project_search; got {names:?}"
    );

    let _ = child.kill();
}
