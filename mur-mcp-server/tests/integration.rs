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

/// RAII guard that ensures a spawned child process is killed and reaped even
/// if the test panics or returns early, avoiding zombie processes.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[test]
fn test_initialize_and_list_tools() {
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

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
    // Notifications receive no response per JSON-RPC, so do not read one.

    // List tools
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
    );
    let resp = read_response(&mut stdout);
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 20, "Expected 20 tools");

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
    assert!(names.contains(&"video_analyze"));
    assert!(names.contains(&"watch_start"));
    assert!(names.contains(&"watch_stop"));
    assert!(names.contains(&"watch_mute"));
    assert!(names.contains(&"watch_status"));
    assert!(names.contains(&"mur_compress"));
    assert!(names.contains(&"mur_retrieve"));
    assert!(names.contains(&"mur_compress_stats"));
    assert!(names.contains(&"parallel_jobs"));
    assert!(names.contains(&"mur_job_status"));
}

#[test]
fn test_tools_list_response_under_token_budget() {
    // Verify tools/list JSON stays under ~5000 tokens (~25,000 chars).
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

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
    // Notifications receive no response per JSON-RPC, so do not read one.
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
}

#[test]
fn lists_project_search_tool() {
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

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
    // Notifications receive no response per JSON-RPC, so do not read one.

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
}

#[test]
fn calls_mur_compress_tool() {
    // Isolate the CCR store in a throwaway MUR_HOME for the child process.
    let home = tempfile::tempdir().unwrap();
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
            .env("MUR_HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

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
    // Notifications receive no response per JSON-RPC, so do not read one.

    // A long search-style payload that should compress and offload.
    let mut lines = Vec::new();
    for i in 0..40 {
        lines.push(format!("src/f{i}.rs:{i}:token number {i}"));
    }
    let content = lines.join("\n");
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "mur_compress", "arguments": { "content": content, "query": "number 7" } }
    })
    .to_string();
    send_request(&mut stdin, &req);
    let resp = read_response(&mut stdout);

    // The tool's JSON result is embedded in the MCP content array; rather than
    // depend on the exact nesting, assert the markers appear anywhere in the response.
    let resp_str = serde_json::to_string(&resp).unwrap();
    assert!(
        resp_str.contains("tokens_saved"),
        "mur_compress result missing tokens_saved: {resp_str}"
    );
    assert!(
        resp_str.contains("hash="),
        "mur_compress result should include a retrieval-hash note: {resp_str}"
    );
}

#[test]
fn parallel_jobs_rejects_empty_jobs() {
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_mur-mcp-server"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
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

    // Empty jobs array -> tool returns an error envelope (isError), never panics.
    send_request(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"parallel_jobs","arguments":{"jobs":[],"agent":"rustsmith"}}}"#,
    );
    let resp = read_response(&mut stdout);
    let resp_str = serde_json::to_string(&resp).unwrap();
    assert!(
        resp_str.contains("isError") || resp_str.to_lowercase().contains("error"),
        "empty jobs should yield an error envelope: {resp_str}"
    );
}
