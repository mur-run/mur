#![no_main]
use libfuzzer_sys::fuzz_target;

// decode_frame reads a 4-byte big-endian length prefix then the payload.
// It must never panic on truncated, oversized, or malformed input.
fuzz_target!(|data: &[u8]| {
    let _ = mur_agent_runtime::transport::noise::decode_frame(data);
});
