//! JSON compressor. Reformat: minify. Offload (SmartCrusher-lite): recursively
//! walk the tree, collapsing large nested arrays (schema keys + BM25/head
//! sample + count) and eliding over-long string leaves, stashing the full
//! original document once behind a single hash so `mur_retrieve` can pull it
//! back byte-for-byte.

use crate::bm25::bm25_rank;
use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

const MIN_ARRAY_FOR_COLLAPSE: usize = 4;
const MAX_DEPTH: usize = 64;
const HASH_SENTINEL: &str = "__MUR_HASH__";

/// Walk state: collected offload items + which transforms fired.
struct Walk<'a> {
    ctx: &'a CompressCtx<'a>,
    tok: &'a dyn TokenCounter,
    items: Vec<String>,
    collapsed_array: bool,
    elided_string: bool,
}

impl Walk<'_> {
    fn visit(&mut self, v: &mut serde_json::Value, depth: usize) {
        if depth >= MAX_DEPTH {
            return;
        }
        match v {
            serde_json::Value::Array(arr) => {
                let sample_n = self.ctx.config.protect_head_lines.min(arr.len());
                if arr.len() >= MIN_ARRAY_FOR_COLLAPSE && arr.len() > sample_n {
                    *v = self.collapse_array(std::mem::take(arr), sample_n);
                    self.collapsed_array = true;
                } else {
                    for item in arr {
                        self.visit(item, depth + 1);
                    }
                }
            }
            serde_json::Value::Object(map) => {
                for (_, val) in map.iter_mut() {
                    self.visit(val, depth + 1);
                }
            }
            serde_json::Value::String(s) => {
                let max = self.ctx.config.json.max_string_tokens;
                if self.tok.count(s) >= max {
                    self.items.push(s.clone());
                    // ~2 chars/token: conservative head slice, char-boundary safe.
                    let head: String = s.chars().take(max * 2).collect();
                    let elided = s.chars().count().saturating_sub(head.chars().count());
                    *v = serde_json::Value::String(format!(
                        "{head}...[{elided} chars elided, hash={HASH_SENTINEL}]"
                    ));
                    self.elided_string = true;
                }
            }
            _ => {}
        }
    }

    /// Replace a large array with {_schema,_total,_shown,sample,_note}.
    fn collapse_array(
        &mut self,
        arr: Vec<serde_json::Value>,
        sample_n: usize,
    ) -> serde_json::Value {
        let keys: Vec<String> = match arr.first() {
            Some(serde_json::Value::Object(m)) => m.keys().cloned().collect(),
            _ => Vec::new(),
        };
        let serialized: Vec<String> = arr
            .iter()
            .map(|x| serde_json::to_string(x).unwrap_or_default())
            .collect();

        // Query-aware sampling: BM25 top-N in original order; else head-N.
        let mut idx: Vec<usize> = match self.ctx.query {
            Some(q) => {
                let mut top: Vec<usize> = bm25_rank(q, &serialized)
                    .into_iter()
                    .take(sample_n)
                    .map(|(i, _)| i)
                    .collect();
                if top.is_empty() {
                    (0..sample_n).collect()
                } else {
                    top.sort_unstable();
                    top
                }
            }
            None => (0..sample_n).collect(),
        };
        idx.dedup();
        let sample: Vec<serde_json::Value> = idx.iter().map(|&i| arr[i].clone()).collect();

        self.items.extend(serialized);
        serde_json::json!({
            "_schema": keys,
            "_total": arr.len(),
            "_shown": sample.len(),
            "sample": sample,
            "_note": format!("{} rows collapsed; full array hash={HASH_SENTINEL}", arr.len() - sample.len()),
        })
    }
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let mut val: serde_json::Value =
        serde_json::from_str(content.trim()).map_err(|e| CompressError::Parse(e.to_string()))?;
    let minified = serde_json::to_string(&val).map_err(|e| CompressError::Parse(e.to_string()))?;
    let transforms = vec!["json.minify".to_string()];

    let mut walk = Walk {
        ctx,
        tok,
        items: Vec::new(),
        collapsed_array: false,
        elided_string: false,
    };
    walk.visit(&mut val, 0);

    if !walk.collapsed_array && !walk.elided_string {
        return Ok(CompressOutput {
            compressed: minified,
            hash: None,
            transforms,
        });
    }

    // Offload the whole original once; every note points at this hash.
    let hash = store
        .put_original(content, walk.items, ContentType::Json, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    let mut transforms = transforms;
    if walk.collapsed_array {
        transforms.push("json.deep_collapse".to_string());
    }
    if walk.elided_string {
        transforms.push("json.string_elide".to_string());
    }
    let compressed = serde_json::to_string(&val)
        .unwrap_or(minified)
        .replace(HASH_SENTINEL, &hash);

    Ok(CompressOutput {
        compressed,
        hash: Some(hash),
        transforms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    fn store_and_ctx(_cfg: &CompressConfig) -> (tempfile::TempDir, CcrStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        (dir, store)
    }

    #[test]
    fn collapses_long_array() {
        let cfg = CompressConfig {
            protect_head_lines: 2,
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
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
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let out = compress("{\n  \"a\": 1\n}", &ctx, &store, &HeuristicCounter).unwrap();
        assert_eq!(out.compressed, r#"{"a":1}"#);
        assert!(out.hash.is_none());
    }

    #[test]
    fn collapses_nested_array() {
        let cfg = CompressConfig {
            protect_head_lines: 2,
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let input =
            r#"{"ok":true,"results":[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5},{"id":6}]}"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some(), "nested array must trigger offload");
        assert!(out.compressed.contains("_total"));
        assert!(out.transforms.iter().any(|t| t == "json.deep_collapse"));
        // retrieve returns the original byte-for-byte
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.original_text, input);
    }

    #[test]
    fn elides_long_string_leaf() {
        let cfg = CompressConfig {
            json: crate::config::JsonCfg {
                max_string_tokens: 10,
            },
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let long = "word ".repeat(200);
        let input = serde_json::json!({"content": long}).to_string();
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.transforms.iter().any(|t| t == "json.string_elide"));
        assert!(out.compressed.len() < input.len() / 2);
        assert!(out.compressed.contains("elided"));
    }

    #[test]
    fn query_picks_relevant_sample() {
        let cfg = CompressConfig {
            protect_head_lines: 2,
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: Some("zebra"),
            config: &cfg,
        };
        let mut rows: Vec<serde_json::Value> = (0..10)
            .map(|i| serde_json::json!({"id": i, "name": "common"}))
            .collect();
        rows.push(serde_json::json!({"id": 99, "name": "zebra special"}));
        let input = serde_json::Value::Array(rows).to_string();
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(
            out.compressed.contains("zebra"),
            "BM25 sample must include the query hit"
        );
    }

    #[test]
    fn depth_cap_degrades_to_minify() {
        let cfg = CompressConfig::default();
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        // 80 levels of nesting, no large arrays: must not panic, plain minify.
        let mut s = String::new();
        for _ in 0..80 {
            s.push_str(r#"{"a":"#);
        }
        s.push('1');
        for _ in 0..80 {
            s.push('}');
        }
        let out = compress(&s, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_none());
    }

    #[test]
    fn no_sentinel_leaks_into_output() {
        let cfg = CompressConfig {
            protect_head_lines: 2,
            ..Default::default()
        };
        let (_d, store) = store_and_ctx(&cfg);
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let input = r#"{"results":[{"id":1},{"id":2},{"id":3},{"id":4},{"id":5}]}"#;
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(!out.compressed.contains("__MUR_HASH__"));
    }
}
