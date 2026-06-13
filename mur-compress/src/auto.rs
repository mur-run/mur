//! Auto-compression facade: a size-gated wrapper over `CompressEngine::compress`
//! plus a shared retrieval envelope, used by every LLM-facing call site
//! (MCP tool outputs, agent-runtime `post_tool_use`). Config-agnostic: callers
//! pass `min_tokens` and check the `auto.*` flags themselves.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::CompressEngine;

/// `auto:` section of `compress.yaml`. Controls *automatic* compression at
/// LLM-facing call sites. The manual `mur_compress`/`mur_retrieve` tools are
/// unaffected by these flags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoCfg {
    /// Master switch for all automatic compression.
    pub enabled: bool,
    /// Outputs counting fewer than this many tokens are never auto-compressed.
    pub min_tokens: usize,
    /// Surface 1: compress MCP tool outputs.
    pub mcp: bool,
    /// Surface 2: compress agent-runtime `post_tool_use` outputs.
    pub agent_runtime: bool,
}

impl Default for AutoCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            min_tokens: 1500,
            mcp: true,
            agent_runtime: true,
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
}
