//! JSON compressor. Reformat: minify. Offload (SmartCrusher-lite): for a long
//! top-level array, emit schema keys + first-N sample + count; stash full array.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

const MIN_ARRAY_FOR_COLLAPSE: usize = 4;

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let val: serde_json::Value =
        serde_json::from_str(content.trim()).map_err(|e| CompressError::Parse(e.to_string()))?;
    let mut transforms = vec!["json.minify".to_string()];
    let minified = serde_json::to_string(&val).map_err(|e| CompressError::Parse(e.to_string()))?;
    let cfg = ctx.config;

    if let serde_json::Value::Array(arr) = &val {
        let sample_n = cfg.protect_head_lines.min(arr.len());
        if arr.len() >= MIN_ARRAY_FOR_COLLAPSE && arr.len() > sample_n {
            let keys: Vec<String> = match arr.first() {
                Some(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
                _ => Vec::new(),
            };
            let items: Vec<String> = arr
                .iter()
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .collect();
            let hash = store
                .put_original(content, items, ContentType::Json, tok)
                .map_err(|e| CompressError::Store(e.to_string()))?;
            transforms.push("json.row_collapse".to_string());

            let sample = serde_json::Value::Array(arr[..sample_n].to_vec());
            let body = serde_json::json!({
                "_schema": keys,
                "_total": arr.len(),
                "_shown": sample_n,
                "sample": sample,
                "_note": format!("{} rows collapsed; full array hash={}", arr.len() - sample_n, hash),
            });
            return Ok(CompressOutput {
                compressed: serde_json::to_string(&body).unwrap_or(minified),
                hash: Some(hash),
                transforms,
            });
        }
    }

    Ok(CompressOutput {
        compressed: minified,
        hash: None,
        transforms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn collapses_long_array() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 2;
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let input = r#"[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6}]"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("_total"));
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.item_count, 6);
    }

    #[test]
    fn minifies_short_json() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let out = compress("{\n  \"a\": 1\n}", &ctx, &store, &HeuristicCounter).unwrap();
        assert_eq!(out.compressed, r#"{"a":1}"#);
        assert!(out.hash.is_none());
    }
}
