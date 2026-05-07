#![no_main]
use libfuzzer_sys::fuzz_target;

// MCP speaks JSON-RPC 2.0. Feed arbitrary bytes into the shared JSON parser.
// A panic here means the parser has an unwrap() on hostile input.
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = serde_json::from_str::<serde_json::Value>(s);
    }
});
