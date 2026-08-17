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

/// Scan a tool-output JSON `value` for embedded error signals and return how
/// many were found. Recognises the three shapes a failed tool result can take
/// as it flows to an LLM-facing surface:
/// - an object with `"is_error": true` or `"ok": false`,
/// - an object whose `"error"` field is a non-null value,
/// - any string (top-level, array element, or object field) beginning with the
///   runtime's `"tool error:"` prefix (case-insensitive, leading ws tolerated).
///
/// Used to (a) refuse to offload error-bearing results and (b) annotate the
/// placeholder when a bulk offload is otherwise unavoidable, so a compressed
/// result is never mistaken for success before `mur_retrieve`.
pub fn tool_error_count(value: &Value) -> usize {
    fn str_is_tool_error(s: &str) -> bool {
        s.trim_start()
            .to_ascii_lowercase()
            .starts_with("tool error:")
    }
    match value {
        Value::String(s) => usize::from(str_is_tool_error(s)),
        Value::Array(items) => items.iter().map(tool_error_count).sum(),
        Value::Object(map) => {
            let mut n = 0;
            if map.get("is_error") == Some(&Value::Bool(true)) {
                n += 1;
            }
            if map.get("ok") == Some(&Value::Bool(false)) {
                n += 1;
            }
            if let Some(e) = map.get("error")
                && !e.is_null()
            {
                n += 1;
            }
            // Recurse into values so a wrapper like {stdout: "tool error: ..."}
            // or an array of results is still counted; skip the flag keys we
            // already scored to avoid double counting.
            for (k, v) in map {
                if k == "is_error" || k == "ok" || k == "error" {
                    continue;
                }
                n += tool_error_count(v);
            }
            n
        }
        _ => 0,
    }
}

/// True iff `value` carries any error signal (see [`tool_error_count`]).
pub fn has_tool_error(value: &Value) -> bool {
    tool_error_count(value) > 0
}

/// The warning that must precede any offloaded result bundling tool errors,
/// so a failure can't read as success until someone calls `mur_retrieve`.
/// Shared by the object envelope's `note` and the string form's prefix.
pub fn error_warning(error_count: usize) -> String {
    format!(
        "WARNING: this offloaded result contains {error_count} tool error(s); \
         do NOT treat it as success — call mur_retrieve to inspect."
    )
}

/// Model-readable hint describing how to recover the full content. When
/// `error_count > 0` the note leads with a hard warning so the offloaded
/// result is never read as success before retrieval.
pub fn retrieval_note_with_errors(
    hash: Option<&str>,
    query: Option<&str>,
    error_count: usize,
) -> String {
    let base = retrieval_note(hash, query);
    if error_count > 0 {
        format!("{} {base}", error_warning(error_count))
    } else {
        base
    }
}

/// Model-readable hint describing how to recover the full content.
///
/// Names both routes on purpose. The `mur_retrieve` tool exists only where the
/// mur MCP server is connected; a plain Claude Code CLI session has to shell
/// out to the binary, and a note that mentions only the tool sends that reader
/// looking for something it does not have.
pub fn retrieval_note(hash: Option<&str>, query: Option<&str>) -> String {
    match hash {
        Some(h) => match query {
            Some(q) => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\" (optionally query=\"{q}\"), or run `mur retrieve {h}`, for the full result."
            ),
            None => format!(
                "Large output compressed; original stored. Call mur_retrieve with hash=\"{h}\", or run `mur retrieve {h}`, for the full result."
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

/// True if `value` is, or anywhere contains, a non-text MCP content block.
///
/// Per the MCP spec a content block's `type` is one of
/// `text | image | audio | resource | resource_link`. Everything but `text`
/// carries a binary or reference payload. Base64 image data is high-entropy, so
/// running it through the text compressors yields a truncated data URI that is
/// neither readable nor still an image block — the caller (a model driving a
/// GUI, reading a rendered chart) goes blind, and `mur_retrieve` can only hand
/// back the raw base64, which is not an image block either. Pass them through.
///
/// The walk descends into object values so a wrapper like
/// `{"content": [{"type":"image",...}]}` is caught too — otherwise the `Object`
/// arm below would pick that array as its largest field and compress it.
fn holds_non_text_content(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(holds_non_text_content),
        Value::Object(map) => {
            matches!(
                map.get("type").and_then(Value::as_str),
                Some("image" | "audio" | "resource" | "resource_link")
            ) || map.values().any(holds_non_text_content)
        }
        _ => false,
    }
}

/// Compress a tool-output JSON `value` in place, routing by shape so the engine's
/// top-level-array / text compressors actually fire (the JSON compressor only
/// collapses a *top-level* array — see `compressors/json.rs`):
/// - `String` → compress the string (catches log / diff / search / JSON text).
/// - `Array`  → compress the whole array (already top-level).
/// - `Object` → compress its largest array-valued field, splicing a compact
///   summary back in; scalar fields are left untouched.
///
/// Non-text MCP content is passed through untouched — see
/// [`holds_non_text_content`].
///
/// Returns `Some(replacement)` iff compression fired, else `None`.
pub fn auto_compress_value(
    engine: &CompressEngine,
    value: &Value,
    query: Option<&str>,
    min_tokens: usize,
) -> Option<Value> {
    // Guard here rather than in `auto_compress_value_guarded`: this function is
    // reachable directly from both LLM-facing surfaces (`mur-mcp-server`'s
    // `maybe_compress_tool_output` and the `PostToolUse` hook in `mur-core`).
    if holds_non_text_content(value) {
        return None;
    }
    match value {
        Value::String(s) => {
            let o = auto_compress(engine, s, query, min_tokens);
            o.fired.then(|| shaped_replacement(value, &o, query))
        }
        Value::Array(_) => {
            let o = auto_compress(engine, &value.to_string(), query, min_tokens);
            o.fired.then(|| value_replacement(&o, query))
        }
        Value::Object(map) => {
            // Candidate fields are arrays (e.g. MCP list results) or strings
            // (e.g. a real Bash tool_response's `stdout`/`stderr`), since
            // Claude Code's PostToolUse stdin wraps tool output as an object
            // like `{stdout: <string>, stderr: <string>, interrupted: bool}`.
            let key = map
                .iter()
                .filter(|(_, v)| v.is_array() || v.is_string())
                .max_by_key(|(_, v)| v.to_string().len())
                .map(|(k, _)| k.clone())?;
            let field = map.get(&key)?;
            // Pass a string field's raw contents (not its JSON-quoted
            // `to_string()`) so the engine sees the real text to compress;
            // arrays still go through `to_string()` since they're already
            // top-level-array-shaped JSON.
            let o = match field {
                Value::String(s) => auto_compress(engine, s, query, min_tokens),
                _ => auto_compress(engine, &field.to_string(), query, min_tokens),
            };
            if !o.fired {
                return None;
            }
            let mut out = map.clone();
            out.insert(key, shaped_replacement(field, &o, query));
            Some(Value::Object(out))
        }
        _ => None,
    }
}

/// Error-aware wrapper over [`auto_compress_value`]. This is the entry point
/// LLM-facing surfaces (agent-runtime `post_tool_use`, MCP) should use for
/// *tool results*, because offloading a failed result to a hash placeholder
/// hides the failure until `mur_retrieve` — the exact bug where an offloaded
/// `"tool error: ..."` gets mistaken for success.
///
/// Contract:
/// 1. If `is_error` is true (caller's own signal, e.g. `ToolResult.ok == false`)
///    OR the value carries any embedded error signal ([`has_tool_error`]), the
///    result is **passed through unchanged** (`None`) — never offloaded,
///    regardless of size.
/// 2. Otherwise it compresses as normal; if a bulk offload still bundles an
///    error string the caller couldn't split out, the placeholder note is
///    annotated with the error count via [`retrieval_note_with_errors`].
pub fn auto_compress_value_guarded(
    engine: &CompressEngine,
    value: &Value,
    query: Option<&str>,
    min_tokens: usize,
    is_error: bool,
) -> Option<Value> {
    // (1) Never offload an error-bearing result.
    if is_error || has_tool_error(value) {
        return None;
    }
    let replacement = auto_compress_value(engine, value, query, min_tokens)?;
    // (2) Belt-and-braces: if the compressed replacement offloaded to a hash and
    // still embeds an error signal (e.g. an error string buried in an otherwise
    // large, non-flagged field), re-annotate its note so it can't read as success.
    Some(annotate_offload_errors(replacement, value, query))
}

/// If `replacement` is an offloaded envelope (`compressed:true` + `hash`) and the
/// original `value` carried error signals, rewrite the envelope `note` to lead
/// with the error-count warning. Otherwise return `replacement` unchanged.
fn annotate_offload_errors(mut replacement: Value, original: &Value, query: Option<&str>) -> Value {
    let n = tool_error_count(original);
    if n == 0 {
        return replacement;
    }
    // String replacements (a schema-declared string field, see
    // `shaped_replacement`) carry their retrieval note inline rather than in an
    // envelope, so `as_object_mut` would skip them and the warning would be
    // silently dropped — exactly the error-hiding this function exists to stop.
    if let Some(s) = replacement.as_str() {
        if s.contains("mur_retrieve") {
            return Value::String(format!("{}\n\n{s}", error_warning(n)));
        }
        return replacement;
    }
    // Top-level offload envelope.
    if let Some(obj) = replacement.as_object_mut() {
        if obj.get("compressed") == Some(&Value::Bool(true))
            && let Some(h) = obj.get("hash").and_then(|h| h.as_str()).map(String::from)
        {
            obj.insert(
                "note".into(),
                Value::String(retrieval_note_with_errors(Some(&h), query, n)),
            );
            obj.insert("tool_errors".into(), Value::from(n));
            return replacement;
        }
        // Object whose largest field was replaced — envelope or, for a
        // schema-declared string field, the inline-note string form.
        for (_k, v) in obj.iter_mut() {
            if let Some(s) = v.as_str() {
                if s.contains("mur_retrieve") {
                    *v = Value::String(format!("{}\n\n{s}", error_warning(n)));
                }
                continue;
            }
            if let Some(inner) = v.as_object_mut()
                && inner.get("compressed") == Some(&Value::Bool(true))
                && let Some(h) = inner.get("hash").and_then(|h| h.as_str()).map(String::from)
            {
                inner.insert(
                    "note".into(),
                    Value::String(retrieval_note_with_errors(Some(&h), query, n)),
                );
                inner.insert("tool_errors".into(), Value::from(n));
            }
        }
    }
    replacement
}

/// Replacement value for a fired outcome: the retrieval envelope when offloaded,
/// else the densified text.
fn value_replacement(outcome: &AutoOutcome, query: Option<&str>) -> Value {
    match outcome.hash {
        Some(_) => retrieval_envelope(outcome, query),
        None => Value::String(outcome.text.clone()),
    }
}

/// Replacement that preserves `original`'s JSON type.
///
/// Claude Code validates a PostToolUse `updatedToolOutput` against the
/// ORIGINATING tool's output schema, so handing back the object envelope where
/// the tool declared a string (Bash's `stdout`/`stderr`, Edit's and Write's
/// fields) fails validation and the entire compression is discarded — silently,
/// via "using original output". For a string we therefore inline the retrieval
/// note into the text instead of wrapping it, so the hash stays reachable by
/// `mur_retrieve` while the field keeps the type the tool declared.
///
/// Note the asymmetry this creates with [`is_compressed_envelope`]-style
/// guards, which recognise only the object form: nothing here re-enters the
/// compressor, because PostToolUse runs once per tool call on a fresh result.
///
/// Arrays keep the envelope: they reach here from MCP list results, whose
/// output schemas are unconstrained.
fn shaped_replacement(original: &Value, outcome: &AutoOutcome, query: Option<&str>) -> Value {
    match original {
        Value::String(_) if outcome.hash.is_some() => Value::String(format!(
            "{}\n\n{}",
            outcome.text,
            retrieval_note(outcome.hash.as_deref(), query)
        )),
        Value::String(_) => Value::String(outcome.text.clone()),
        _ => value_replacement(outcome, query),
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

    /// An MCP image result, shaped like a real `mcp__computer-use__screenshot`
    /// return and far over `min_tokens` so it would fire without the guard.
    fn mcp_image_result() -> Value {
        serde_json::json!([{
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": "/9j/4AAQSkZJRgABAQAASABIAAD".repeat(4000),
            }
        }])
    }

    #[test]
    fn mcp_image_content_is_never_compressed() {
        let (_dir, eng) = engine();
        let img = mcp_image_result();

        // Sanity: the same payload as plain text is well over the gate, so a
        // `None` here is the guard firing, not the size gate.
        let as_text = Value::String(img.to_string());
        assert!(
            auto_compress_value(&eng, &as_text, None, 1500).is_some(),
            "fixture must be large enough to compress, else this test proves nothing"
        );

        assert!(auto_compress_value(&eng, &img, None, 1500).is_none());
        assert!(auto_compress_value_guarded(&eng, &img, None, 1500, false).is_none());

        // Wrapped one level down: the `Object` arm would otherwise pick the
        // image array as its largest field.
        let wrapped = serde_json::json!({ "content": img, "isError": false });
        assert!(auto_compress_value(&eng, &wrapped, None, 1500).is_none());
    }

    #[test]
    fn text_content_blocks_still_compress() {
        let (_dir, eng) = engine();
        let text = serde_json::json!([{ "type": "text", "text": big_json_array() }]);
        assert!(
            auto_compress_value(&eng, &text, None, 1500).is_some(),
            "the guard must not swallow ordinary text content blocks"
        );
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

    // Real Claude Code PostToolUse stdin wraps Bash tool output as an
    // object: `{stdout: <string>, stderr: <string>, interrupted: bool}`.
    // The Object branch must consider string fields too, not just arrays.
    #[test]
    fn value_object_tool_response_compresses_large_stdout_string() {
        let (_dir, eng) = engine();
        let v = serde_json::json!({
            "stdout": big_json_array(),
            "stderr": "",
            "interrupted": false,
        });
        let out = auto_compress_value(&eng, &v, None, 50).expect("should fire");
        assert_eq!(out["interrupted"], serde_json::json!(false));
        assert_eq!(out["stderr"], serde_json::json!(""));
        // `stdout` is a string in Claude Code's Bash output schema, so the
        // replacement must stay a string — swapping in the object envelope
        // fails schema validation and the compression is thrown away.
        let stdout = out["stdout"]
            .as_str()
            .expect("stdout must stay a string, not become the envelope");
        assert!(stdout.contains("mur_retrieve"), "{stdout}");
    }

    #[test]
    fn value_object_tool_response_small_stdout_returns_none() {
        let (_dir, eng) = engine();
        let v = serde_json::json!({
            "stdout": "ok",
            "stderr": "",
            "interrupted": false,
        });
        assert!(auto_compress_value(&eng, &v, None, 1500).is_none());
    }

    // --- tool_error_count: every shape a failed tool result can take ---

    #[test]
    fn tool_error_count_flags_is_error_true() {
        let v = serde_json::json!({"is_error": true, "content": "boom"});
        assert_eq!(tool_error_count(&v), 1);
        assert!(has_tool_error(&v));
    }

    #[test]
    fn tool_error_count_flags_ok_false() {
        let v = serde_json::json!({"ok": false, "output": "nope"});
        assert_eq!(tool_error_count(&v), 1);
    }

    #[test]
    fn tool_error_count_flags_non_null_error_field() {
        let v = serde_json::json!({"error": "old_string not found"});
        assert_eq!(tool_error_count(&v), 1);
        // A null error field is not an error.
        let ok = serde_json::json!({"error": Value::Null, "output": "fine"});
        assert_eq!(tool_error_count(&ok), 0);
    }

    #[test]
    fn tool_error_count_flags_tool_error_prefix_string() {
        // The exact bug: edit_file returns "tool error: ..." as a plain string.
        let v = serde_json::json!("tool error: invalid input: old_string not found");
        assert_eq!(tool_error_count(&v), 1);
        // Case- and leading-whitespace-insensitive.
        let v2 = serde_json::json!("  TOOL ERROR: boom");
        assert_eq!(tool_error_count(&v2), 1);
    }

    #[test]
    fn tool_error_count_finds_nested_tool_error_in_stdout() {
        // Wrapper object like a Bash tool_response burying the failure.
        let v = serde_json::json!({
            "stdout": "tool error: old_string not found",
            "stderr": "",
            "interrupted": false,
        });
        assert_eq!(tool_error_count(&v), 1);
    }

    #[test]
    fn tool_error_count_sums_array_of_results() {
        let v = serde_json::json!([
            {"ok": true, "output": "fine"},
            {"is_error": true},
            "tool error: second failure",
        ]);
        assert_eq!(tool_error_count(&v), 2);
    }

    #[test]
    fn tool_error_count_clean_result_is_zero() {
        let v = serde_json::json!({"ok": true, "output": "all good", "error": Value::Null});
        assert_eq!(tool_error_count(&v), 0);
        assert!(!has_tool_error(&v));
    }

    // --- auto_compress_value_guarded: errors never offload; placeholders warn ---

    #[test]
    fn guarded_passes_through_when_caller_flags_error() {
        let (_dir, eng) = engine();
        // A large, otherwise-compressible payload — but the caller says it failed.
        let v = Value::String(big_json_array());
        assert!(
            auto_compress_value_guarded(&eng, &v, None, 100, true).is_none(),
            "is_error=true must never offload, regardless of size"
        );
    }

    #[test]
    fn guarded_passes_through_when_payload_embeds_tool_error() {
        let (_dir, eng) = engine();
        // Big enough to normally offload, but stdout carries a tool error.
        let mut buried = big_json_array();
        buried.push_str("\ntool error: old_string not found");
        let v = serde_json::json!({"stdout": buried, "stderr": "", "interrupted": false});
        assert!(
            auto_compress_value_guarded(&eng, &v, None, 100, false).is_none(),
            "embedded tool error must never offload even if caller flag is false"
        );
    }

    #[test]
    fn guarded_offloads_clean_large_payload_normally() {
        let (_dir, eng) = engine();
        let v = Value::String(big_json_array());
        let out = auto_compress_value_guarded(&eng, &v, None, 100, false)
            .expect("clean large payload should still compress");
        // A string input keeps the string shape; the hash rides along in the
        // inline retrieval note.
        let s = out.as_str().expect("string input keeps string shape");
        assert!(s.contains("mur_retrieve"), "{s}");
        // No error annotation on a clean result.
        assert!(!s.contains("WARNING"), "{s}");
    }

    #[test]
    fn guarded_annotates_offload_that_still_bundles_an_error() {
        let (_dir, eng) = engine();
        // Object whose largest field is a clean big array (so it offloads),
        // while a sibling scalar carries the error signal the caller couldn't
        // split out. has_tool_error would gate the whole thing — so exercise
        // annotate_offload_errors directly on a produced envelope + error origin.
        let clean = serde_json::json!({"results": (0..3000).map(|i| serde_json::json!({"id": i})).collect::<Vec<_>>()});
        let replacement = auto_compress_value(&eng, &clean, None, 50).expect("should fire");
        let with_err = serde_json::json!({"is_error": true, "results": clean["results"].clone()});
        let annotated = annotate_offload_errors(replacement, &with_err, None);
        let env = &annotated["results"];
        assert_eq!(env["compressed"], serde_json::json!(true));
        assert_eq!(env["tool_errors"], serde_json::json!(1));
        assert!(
            env["note"].as_str().unwrap().contains("WARNING"),
            "annotated note must warn against treating as success"
        );
        assert!(env["note"].as_str().unwrap().contains("do NOT treat"));
    }

    #[test]
    fn retrieval_note_with_errors_leads_with_warning() {
        let note = retrieval_note_with_errors(Some("abc123"), Some("q"), 3);
        assert!(note.contains("3 tool error"));
        assert!(note.contains("do NOT treat"));
        assert!(note.contains("mur_retrieve"));
        // Zero errors → plain note, no warning.
        let plain = retrieval_note_with_errors(Some("abc123"), None, 0);
        assert!(!plain.contains("WARNING"));
    }
}
