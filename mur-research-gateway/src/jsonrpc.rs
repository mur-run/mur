// mur-research-gateway/src/jsonrpc.rs
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

/// JSON-RPC 2.0 request (what the client sends us).
#[derive(Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    // Unused until tools/call gains real dispatch logic (Tasks 2-5).
    #[allow(dead_code)]
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

/// Read one JSON-RPC request from stdin. Blocks until a complete line.
/// Returns None if stdin closes.
pub fn read_request() -> Option<Request> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) => None, // EOF
        Ok(_) => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return read_request(); // skip blank lines
            }
            match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => {
                    tracing::debug!(method = %req.method, id = ?req.id, "received request");
                    Some(req)
                }
                Err(e) => {
                    tracing::warn!(error = %e, raw = %trimmed, "failed to parse request");
                    // Return a parse-error-shaped request so the caller can respond
                    Some(Request {
                        jsonrpc: "2.0".into(),
                        id: None,
                        method: String::new(),
                        params: None,
                    })
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "stdin read error");
            None
        }
    }
}

/// Write one JSON-RPC response to stdout. One line per response.
pub fn write_response(resp: &Response) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let json = serde_json::to_string(resp).unwrap_or_else(|e| {
        serde_json::to_string(&Response::error(
            None,
            -32700,
            format!("failed to serialize response: {}", e),
        ))
        .unwrap()
    });
    writeln!(handle, "{}", json).ok();
    handle.flush().ok();
    tracing::debug!(json = %json, "sent response");
}

/// Write a JSON-RPC notification (no id, no response expected).
#[allow(dead_code)]
pub fn write_notification(method: &str, params: Value) {
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", serde_json::to_string(&notif).unwrap()).ok();
    handle.flush().ok();
}
