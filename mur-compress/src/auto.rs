//! Auto-compression facade: a size-gated wrapper over `CompressEngine::compress`
//! plus a shared retrieval envelope, used by every LLM-facing call site
//! (MCP tool outputs, agent-runtime `post_tool_use`). Config-agnostic: callers
//! pass `min_tokens` and check the `auto.*` flags themselves.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CompressEngine;

/// Absolute floor for `auto.min_tokens`, regardless of what `compress.yaml`
/// configures. Mirrors headroom's under-500-token skip: outputs this small
/// are cheap to read as-is and not worth the compression + retrieval-hop
/// overhead, so no configured value can push the gate below this.
pub const MIN_TOKENS_FLOOR: usize = 500;

/// Default `auto.min_tokens` when `compress.yaml` doesn't set one. Lowered
/// from the original 1500 so mid-size (multi-KB) tool outputs — which used
/// to slip through uncompressed in high-output sessions — now qualify,
/// while staying above [`MIN_TOKENS_FLOOR`].
pub const DEFAULT_MIN_TOKENS: usize = 800;

/// `auto:` section of `compress.yaml`. Controls *automatic* compression at
/// LLM-facing call sites. The manual `mur_compress`/`mur_retrieve` tools are
/// unaffected by these flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCfg {
    /// Master switch for all automatic compression.
    pub enabled: bool,
    /// Outputs counting fewer than this many tokens are never auto-compressed.
    /// Defaults to [`DEFAULT_MIN_TOKENS`]; whatever value is configured here
    /// is still clamped up to [`MIN_TOKENS_FLOOR`] at compression time, so
    /// tiny outputs can never be auto-compressed even via misconfiguration.
    pub min_tokens: usize,
    /// Surface 1: compress MCP tool outputs.
    pub mcp: bool,
    /// Surface 2: compress agent-runtime `post_tool_use` outputs.
    pub agent_runtime: bool,
    /// Surface 3: compress Claude Code PostToolUse hook stdout-replacement output.
    pub claude_hook: bool,
}

impl Default for AutoCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            min_tokens: DEFAULT_MIN_TOKENS,
            mcp: true,
            agent_runtime: true,
            claude_hook: true,
        }
    }
}

/// Outcome of an [`auto_compress`] call.
#[derive(Debug, Clone)]
pub struct AutoOutcome {
    /// Compressed text if `fired`, else the original text unchanged.
    pub text: String,
    /// Present only when content was offloaded to the CCR store.
    pub hash: Option<String>,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    /// True iff the original was replaced with something strictly smaller.
    pub fired: bool,
}

/// Size-gated compression. Never errors: any failure or non-payoff returns the
/// original text with `fired: false`. `min_tokens` is the caller's gate; the
/// engine's own `bloat_threshold` is the second gate.
pub fn auto_compress(
    engine: &CompressEngine,
    text: &str,
    query: Option<&str>,
    min_tokens: usize,
) -> AutoOutcome {
    // Clamp the caller/config-supplied gate up to the floor: no configured
    // value can make auto-compression fire on outputs this small.
    let min_tokens = min_tokens.max(MIN_TOKENS_FLOOR);
    let before = engine.count_tokens(text);
    if before < min_tokens {
        return AutoOutcome {
            text: text.to_string(),
            hash: None,
            original_tokens: before,
            compressed_tokens: before,
            fired: false,
        };
    }
    let r = engine.compress(text, query);
    // compress() returns a passthrough (tokens_saved == 0) when it doesn't pay off.
    let fired = r.tokens_saved > 0;
    AutoOutcome {
        text: r.compressed,
        hash: r.hash,
        original_tokens: r.original_tokens,
        compressed_tokens: r.compressed_tokens,
        fired,
    }
}

/// Model-readable hint describing how to recover the full content.
pub fn retrieval_note(hash: Option<&str>, query: Option<&str>) -> String {
    match hash {
        Some(h) => match query {
            Some(q) => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\" (optionally query=\"{q}\") for the full result."
            ),
            None => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\" for the full result."
            ),
        },
        None => "Output densified in place; nothing offloaded.".to_string(),
    }
}

/// Standard envelope wrapping an offloaded (hash-bearing) compressed result.
/// Both surfaces use this so the model always sees one shape.
pub fn retrieval_envelope(outcome: &AutoOutcome, query: Option<&str>) -> Value {
    json!({
        "compressed": true,
        "content": outcome.text,
        "hash": outcome.hash,
        "original_tokens": outcome.original_tokens,
        "compressed_tokens": outcome.compressed_tokens,
        "note": retrieval_note(outcome.hash.as_deref(), query),
    })
}

/// Compress a tool-output JSON `value` in place, routing by shape so the engine's
/// top-level-array / text compressors actually fire (the JSON compressor only
/// collapses a *top-level* array — see `compressors/json.rs`):
/// - `String` → compress the string (catches log / diff / search / JSON text).
/// - `Array`  → compress the whole array (already top-level).
/// - `Object` → compress its largest array-valued field, splicing a compact
///   summary back in; scalar fields are left untouched.
///
/// Returns `Some(replacement)` iff compression fired, else `None`.
pub fn auto_compress_value(
    engine: &CompressEngine,
    value: &Value,
    query: Option<&str>,
    min_tokens: usize,
) -> Option<Value> {
    match value {
        Value::String(s) => {
            let o = auto_compress(engine, s, query, min_tokens);
            o.fired.then(|| value_replacement(&o, query))
        }
        Value::Array(_) => {
            let o = auto_compress(engine, &value.to_string(), query, min_tokens);
            o.fired.then(|| value_replacement(&o, query))
        }
        Value::Object(map) => {
            let key = map
                .iter()
                .filter(|(_, v)| v.is_array())
                .max_by_key(|(_, v)| v.to_string().len())
                .map(|(k, _)| k.clone())?;
            let arr_text = map.get(&key).map(|v| v.to_string()).unwrap_or_default();
            let o = auto_compress(engine, &arr_text, query, min_tokens);
            if !o.fired {
                return None;
            }
            let mut out = map.clone();
            out.insert(key, value_replacement(&o, query));
            Some(Value::Object(out))
        }
        _ => None,
    }
}

/// Replacement value for a fired outcome: the retrieval envelope when offloaded,
/// else the densified text.
fn value_replacement(outcome: &AutoOutcome, query: Option<&str>) -> Value {
    match outcome.hash {
        Some(_) => retrieval_envelope(outcome, query),
        None => Value::String(outcome.text.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompressConfig, CompressEngine};

    fn engine() -> (tempfile::TempDir, CompressEngine) {
        let dir = tempfile::tempdir().unwrap();
        let eng = CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap();
        (dir, eng)
    }

    fn big_json_array() -> String {
        let items: Vec<String> = (0..4000)
            .map(|i| format!("{{\"id\":{i},\"name\":\"item-{i}\",\"value\":{}}}", i * 7))
            .collect();
        format!("[{}]", items.join(","))
    }

    #[test]
    fn small_input_is_gated_out() {
        let (_dir, eng) = engine();
        let out = auto_compress(&eng, "tiny output", None, 1500);
        assert!(!out.fired);
        assert_eq!(out.text, "tiny output");
        assert!(out.hash.is_none());
    }

    // Compile-time invariants on the constants themselves (clippy flags a
    // plain `assert!` on constant operands, so this is a const block instead).
    const _: () = assert!(DEFAULT_MIN_TOKENS > MIN_TOKENS_FLOOR);
    const _: () = assert!(DEFAULT_MIN_TOKENS < 1500);

    #[test]
    fn default_min_tokens_is_lowered_but_above_floor() {
        assert_eq!(AutoCfg::default().min_tokens, DEFAULT_MIN_TOKENS);
    }

    #[test]
    fn config_override_of_min_tokens_is_respected() {
        let (_dir, eng) = engine();
        // A wide gap between the configured minimum and the input's token
        // count proves the override (not the default) is what's applied.
        let out = auto_compress(&eng, &big_json_array(), None, 2_000);
        assert!(
            out.fired,
            "configured min_tokens below the input size should fire"
        );
    }

    #[test]
    fn min_tokens_below_floor_is_clamped_up() {
        let (_dir, eng) = engine();
        // "tiny output" is well under MIN_TOKENS_FLOOR; even a misconfigured
        // min_tokens of 0 must not let it through.
        let out = auto_compress(&eng, "tiny output", None, 0);
        assert!(
            !out.fired,
            "floor must gate out tiny output regardless of config"
        );
    }

    #[test]
    fn large_json_array_fires_and_offloads() {
        let (_dir, eng) = engine();
        let out = auto_compress(&eng, &big_json_array(), None, 100);
        assert!(out.fired, "large json array should compress");
        assert!(
            out.hash.is_some(),
            "json array offload should produce a hash"
        );
        assert!(out.compressed_tokens < out.original_tokens);
    }

    #[test]
    fn gate_uses_min_tokens() {
        let (_dir, eng) = engine();
        let out = auto_compress(&eng, &big_json_array(), None, 1_000_000);
        assert!(!out.fired);
    }

    #[test]
    fn envelope_has_stable_shape() {
        let (_dir, eng) = engine();
        let out = auto_compress(&eng, &big_json_array(), Some("item"), 100);
        let env = retrieval_envelope(&out, Some("item"));
        assert_eq!(env["compressed"], serde_json::json!(true));
        assert!(env["hash"].as_str().is_some());
        assert!(env["note"].as_str().unwrap().contains("mur_retrieve"));
    }

    #[test]
    fn value_object_compresses_largest_array_field() {
        let (_dir, eng) = engine();
        let results: Vec<Value> = (0..3000)
            .map(|i| serde_json::json!({"file": format!("src/f{i}.rs"), "score": 0.5}))
            .collect();
        let v = serde_json::json!({"results": results, "count": 3000});
        let out = auto_compress_value(&eng, &v, Some("f"), 50).expect("should fire");
        assert_eq!(out["count"], serde_json::json!(3000));
        assert_eq!(out["results"]["compressed"], serde_json::json!(true));
        assert!(out["results"]["hash"].as_str().is_some());
    }

    #[test]
    fn value_top_level_array_compresses() {
        let (_dir, eng) = engine();
        let arr: Vec<Value> = (0..3000).map(|i| serde_json::json!({"id": i})).collect();
        let out = auto_compress_value(&eng, &Value::Array(arr), None, 50).expect("should fire");
        assert_eq!(out["compressed"], serde_json::json!(true));
        assert!(out["hash"].as_str().is_some());
    }

    #[test]
    fn value_small_object_returns_none() {
        let (_dir, eng) = engine();
        let v = serde_json::json!({"results": ["a", "b"], "count": 2});
        assert!(auto_compress_value(&eng, &v, None, 1500).is_none());
    }
}
