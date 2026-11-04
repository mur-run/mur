//! Generic fallback: densify (trim trailing ws, collapse blank runs), offload
//! the original to the CCR store, and return a short preview so the model can
//! retrieve the full content when it needs it. Falls back to densify-only on
//! store failure.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

/// Lines to preview before the offload marker.
const PREVIEW_LINES: usize = 3;

pub fn compress(
    content: &str,
    _ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    // Densify in place (same as before).
    let mut densified = String::with_capacity(content.len());
    let mut blank_run = 0;
    for line in content.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        densified.push_str(trimmed);
        densified.push('\n');
    }
    if !content.ends_with('\n') {
        densified.pop();
    }

    // Offload the ORIGINAL (untrimmed) so retrieval is lossless.
    let items: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let hash = match store.put_original(content, items, ContentType::Generic, tok) {
        Ok(h) => h,
        Err(_) => {
            // Store write failed — fall back to densify-only.
            return Ok(CompressOutput {
                compressed: densified,
                hash: None,
                transforms: vec!["fallback.whitespace".into()],
            });
        }
    };

    // Build preview: first N non-empty lines of densified text.
    let preview_lines: Vec<&str> = densified
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(PREVIEW_LINES)
        .collect();
    let total_lines = densified.lines().count();
    let token_count = tok.count(&densified);
    let skipped = total_lines.saturating_sub(preview_lines.len());

    let mut out = preview_lines.join("\n");
    out.push_str(&format!(
        "\n[... {} lines, ~{} tokens archived. Retrieve: hash={}]",
        skipped, token_count, hash
    ));

    Ok(CompressOutput {
        compressed: out,
        hash: Some(hash),
        transforms: vec!["fallback.offload".into()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn offloads_and_shows_preview() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 10, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let input = "line one   \n\n\n\nline two\n\n\n\nline three\nline four\n";
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        // Preview: first 3 non-empty densified lines
        assert!(out.compressed.contains("line one"));
        assert!(out.compressed.contains("line two"));
        assert!(out.compressed.contains("line three"));
        // Marker present + offloaded
        assert!(out.compressed.contains("archived. Retrieve: hash="));
        assert!(out.hash.is_some(), "generic must offload");
        // Full original retrievable
        let entry = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(entry.original_text, input);
    }
}
