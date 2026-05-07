#![no_main]
use libfuzzer_sys::fuzz_target;

// MurCard is defined in mur-core (not in scope here). Parse as generic
// YAML Value instead — this still catches panics from hostile YAML input
// hitting serde_yaml_ng's parser (alias bombs, deep nesting, etc.).
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(s);
    }
});
