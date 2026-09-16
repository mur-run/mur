//! JSON-RPC 2.0 types and stdio framing for MCP.
//!
//! Lives below both `mur-mcp-server` and `mur-agent-runtime` because each
//! serves MCP and neither may depend on the other — `mur-agent-runtime` must
//! not pull in `mur-core` (LanceDB and Arrow in every agent process).
//!
//! Only the protocol is here. The dispatch loop is not: `mur-mcp-server`
//! answers requests, while the runtime's server must also *originate* them
//! (`elicitation/create`, the HITL transport), so the two loops are different
//! shapes and sharing one would fit neither.

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
/// One line off a peer's stream, classified.
///
/// A bidirectional MCP endpoint receives both: requests the peer is asking
/// it to serve, and responses to requests it sent (`elicitation/create`).
/// They share one stream and differ only by shape — a request has `method`,
/// a response has `result` or `error`.
#[derive(Debug)]
pub enum Incoming {
    Request(Request),
    /// Raw, because [`Response`] holds `jsonrpc` as a `&'static str` and so
    /// cannot be deserialized into. The caller wants the `id` and the
    /// `result`/`error`, both of which a `Value` gives directly.
    Response(Value),
    /// Neither shape parsed. Kept as the raw line so the caller can log it
    /// and carry on rather than treating a peer's malformed frame as EOF.
    Unparseable(String),
}

/// Read and classify one line. `None` is EOF and only EOF.
///
/// Takes the reader instead of locking stdin, unlike [`read_request`]: a
/// bidirectional endpoint owns its stdin handle on a blocking thread, and a
/// function that grabs the global lock cannot be driven from there.
pub fn read_incoming<R: std::io::BufRead>(r: &mut R) -> Option<Incoming> {
    loop {
        let mut line = String::new();
        match r.read_line(&mut line) {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                    return Some(Incoming::Unparseable(trimmed.to_string()));
                };
                // Shape, not guesswork: a request has `method`, a response
                // has `result` or `error`. Anything with neither is not a
                // JSON-RPC frame we can route.
                if v.get("method").is_some() {
                    return match serde_json::from_value::<Request>(v) {
                        Ok(req) => Some(Incoming::Request(req)),
                        Err(_) => Some(Incoming::Unparseable(trimmed.to_string())),
                    };
                }
                if v.get("result").is_some() || v.get("error").is_some() {
                    return Some(Incoming::Response(v));
                }
                return Some(Incoming::Unparseable(trimmed.to_string()));
            }
            Err(e) => {
                tracing::error!(error = %e, "read error");
                return None;
            }
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn read(s: &str) -> Option<Incoming> {
        read_incoming(&mut Cursor::new(s.as_bytes().to_vec()))
    }

    #[test]
    fn a_request_is_read_as_a_request() {
        let got = read(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
        assert!(matches!(got, Some(Incoming::Request(r)) if r.method == "tools/list"));
    }

    #[test]
    fn a_response_is_not_mistaken_for_a_broken_request() {
        // The whole reason this function exists. `read_request` turns this
        // line into a parse-error request, which is silently wrong for a
        // bidirectional endpoint — the answer to its own elicitation would
        // be read as the peer asking it something.
        let got = read(r#"{"jsonrpc":"2.0","id":"e-1","result":{"action":"accept"}}"#);
        assert!(matches!(got, Some(Incoming::Response(_))), "{got:?}");
    }

    #[test]
    fn an_error_response_is_still_a_response() {
        let got = read(r#"{"jsonrpc":"2.0","id":"e-1","error":{"code":-32601,"message":"no"}}"#);
        assert!(matches!(got, Some(Incoming::Response(_))), "{got:?}");
    }

    #[test]
    fn garbage_is_reported_not_treated_as_eof() {
        // EOF ends the loop. A malformed frame must not, or one bad line
        // from the peer would look like the peer hanging up.
        assert!(matches!(read("not json at all\n"), Some(Incoming::Unparseable(_))));
    }

    #[test]
    fn eof_is_none() {
        assert!(read("").is_none());
    }
}
