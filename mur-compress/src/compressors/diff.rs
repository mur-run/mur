//! Diff compressor. Reformat+Offload: keep headers and changed (+/-) lines;
//! collapse long unchanged-context runs (>3 lines) to a marker; stash full diff.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

const CONTEXT_KEEP: usize = 3;

fn is_header(l: &str) -> bool {
    l.starts_with("diff ")
        || l.starts_with("@@")
        || l.starts_with("--- ")
        || l.starts_with("+++ ")
        || l.starts_with("index ")
}

pub fn compress(
    content: &str,
    _ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let lines: Vec<&str> = content.lines().collect();
    let mut transforms = vec!["diff.trim_context".to_string()];
    let mut kept: Vec<String> = Vec::new();
    let mut context_run = 0usize;
    let mut dropped = 0usize;

    for l in &lines {
        let change = l.starts_with('+') || l.starts_with('-');
        let header = is_header(l);
        let context = !change && !header;
        if context {
            context_run += 1;
            if context_run <= CONTEXT_KEEP {
                kept.push((*l).to_string());
            } else {
                dropped += 1;
            }
        } else {
            if context_run > CONTEXT_KEEP {
                kept.push(format!(
                    "... ({} unchanged lines)",
                    context_run - CONTEXT_KEEP
                ));
            }
            context_run = 0;
            kept.push((*l).to_string());
        }
    }
    if context_run > CONTEXT_KEEP {
        kept.push(format!(
            "... ({} unchanged lines)",
            context_run - CONTEXT_KEEP
        ));
    }

    if dropped == 0 {
        return Ok(CompressOutput {
            compressed: content.to_string(),
            hash: None,
            transforms,
        });
    }

    let items: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    let hash = store
        .put_original(content, items, ContentType::GitDiff, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("diff.offload".to_string());

    let mut body = kept.join("\n");
    body.push_str(&format!(
        "\n[diff context trimmed; {dropped} lines offloaded. Full diff: hash={hash}]"
    ));
    Ok(CompressOutput {
        compressed: body,
        hash: Some(hash),
        transforms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn trims_large_unchanged_context() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let mut s = String::from("diff --git a/x b/x\n@@ -1,20 +1,20 @@\n");
        for _ in 0..20 {
            s.push_str(" unchanged\n");
        }
        s.push_str("+added line\n");
        let out = compress(&s, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.compressed.contains("+added line"));
        assert!(out.compressed.contains("unchanged lines)"));
        assert!(out.hash.is_some());
    }
}
