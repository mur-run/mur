//! Generic fallback: lossless-ish densify (trim trailing ws, collapse blank
//! runs). No offload — prose/code are left intact in v1.

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput};

pub fn compress(
    content: &str,
    _ctx: &CompressCtx,
    _store: &CcrStore,
    _tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let mut out = String::with_capacity(content.len());
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
        out.push_str(trimmed);
        out.push('\n');
    }
    if !content.ends_with('\n') {
        out.pop();
    }
    Ok(CompressOutput {
        compressed: out,
        hash: None,
        transforms: vec!["fallback.whitespace".into()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CompressConfig;
    use crate::tokenizer::HeuristicCounter;

    #[test]
    fn collapses_blank_runs_and_trailing_ws() {
        let cfg = CompressConfig::default();
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 10, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let input = "line one   \n\n\n\nline two\n";
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert_eq!(out.compressed, "line one\n\nline two\n");
        assert!(out.hash.is_none());
    }
}
