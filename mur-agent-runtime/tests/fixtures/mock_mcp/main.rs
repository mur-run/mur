use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => return,
        };
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req["id"].clone();
        let method = req["method"].as_str().unwrap_or("");
        let result = match method {
            "initialize" => serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "mock_mcp", "version": "0.0.1"}
            }),
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "echo",
                    "description": "echoes",
                    "inputSchema": {"type": "object"}
                }]
            }),
            "tools/call" => {
                let args = req["params"]["arguments"].clone();
                serde_json::json!({
                    "content": [{"type": "text", "text": format!("echo: {args}")}]
                })
            }
            _ => serde_json::json!(null),
        };
        let resp = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        writeln!(stdout, "{}", resp).unwrap();
        stdout.flush().unwrap();
    }
}
