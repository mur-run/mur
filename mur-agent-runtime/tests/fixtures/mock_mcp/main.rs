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
                }, {
                    "name": "connect",
                    "description": "CONNECTs to `host` through HTTPS_PROXY, returns the status line",
                    "inputSchema": {"type": "object"}
                }]
            }),
            "tools/call" if req["params"]["name"] == "connect" => {
                let host = req["params"]["arguments"]["host"].as_str().unwrap_or("");
                serde_json::json!({
                    "content": [{"type": "text", "text": connect_via_proxy(host)}]
                })
            }
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

/// What a real MCP child does when it fetches: CONNECT through the proxy the
/// runtime handed it in `HTTPS_PROXY` (`http://<token>:x@<addr>`).
fn connect_via_proxy(host: &str) -> String {
    use base64::Engine;
    use std::io::Read;
    let Ok(url) = std::env::var("HTTPS_PROXY") else {
        return "no HTTPS_PROXY".into();
    };
    let rest = url.trim_start_matches("http://");
    let Some((cred, addr)) = rest.rsplit_once('@') else {
        return format!("unparseable proxy url {url}");
    };
    let Ok(mut s) = std::net::TcpStream::connect(addr) else {
        return format!("cannot reach proxy {addr}");
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(cred);
    let _ = write!(
        s,
        "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\nProxy-Authorization: Basic {b64}\r\n\r\n"
    );
    let mut buf = [0u8; 256];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n])
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}
