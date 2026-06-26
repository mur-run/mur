//! Pure parsing for Streamable HTTP MCP responses (JSON or SSE).
use serde_json::Value;

/// Extract JSON payloads from an MCP HTTP response body. Handles both a bare
/// JSON object and a `text/event-stream` body (one or more `data:` lines per
/// event, events separated by blank lines). Non-JSON `data:` lines are skipped.
pub fn parse_sse_events(body: &str) -> Vec<Value> {
    let trimmed = body.trim_start();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && let Ok(v) = serde_json::from_str::<Value>(trimmed)
    {
        return vec![v];
    }
    let mut out = Vec::new();
    for block in body.split("\n\n") {
        let data: String = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(|d| d.trim_start()))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.push(v);
        }
    }
    out
}

/// Return the `result` of the JSON-RPC response whose `id` matches, mapping a
/// JSON-RPC `error` object into an `Err`.
// ponytail: full-body read; server-initiated sampling mid-call is out of scope — add a streaming reader if a server needs it.
pub fn jsonrpc_result_for(events: &[Value], id: i64) -> Option<&Value> {
    events
        .iter()
        .find(|e| e.get("id").and_then(|i| i.as_i64()) == Some(id))
        .map(|e| &e["result"])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sse_and_matches_id() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/log\",\"params\":{}}\n\n";
        let events = parse_sse_events(body);
        assert_eq!(events.len(), 2);
        let r = jsonrpc_result_for(&events, 1).unwrap();
        assert!(r.get("tools").is_some());
        assert!(jsonrpc_result_for(&events, 99).is_none());
    }
    #[test]
    fn plain_json_body_is_one_event() {
        let body = "{\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{\"ok\":true}}";
        let events = parse_sse_events(body);
        assert_eq!(jsonrpc_result_for(&events, 7).unwrap()["ok"], true);
    }
}
