//! Log compressor. Reformat: collapse consecutive duplicate lines into
//! "<line>  (xN)". Offload: keep ERROR/WARN/FATAL + head/tail; stash full log.

use std::sync::LazyLock;

use regex::Regex;

use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

static IMPORTANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(ERROR|WARN|WARNING|FATAL|PANIC|FAIL)\b").unwrap());

fn collapse_repeats(lines: &[String]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let mut count = 1;
        while i + count < lines.len() && lines[i + count] == lines[i] {
            count += 1;
        }
        out.push_str(&lines[i]);
        if count > 1 {
            out.push_str(&format!("  (x{count})"));
        }
        out.push('\n');
        i += count;
    }
    out
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let items: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let cfg = ctx.config;
    let mut transforms = vec!["log.collapse_repeats".to_string()];
    let n = items.len();

    let keep_idx: Vec<usize> = (0..n)
        .filter(|&i| {
            i < cfg.protect_head_lines
                || i >= n.saturating_sub(cfg.protect_tail_lines)
                || IMPORTANT.is_match(&items[i])
        })
        .collect();

    if keep_idx.len() >= n {
        return Ok(CompressOutput {
            compressed: collapse_repeats(&items),
            hash: None,
            transforms,
        });
    }

    let hash = store
        .put_original(content, items.clone(), ContentType::BuildLog, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("log.offload".to_string());

    let kept: Vec<String> = keep_idx.iter().map(|&i| items[i].clone()).collect();
    let mut body = collapse_repeats(&kept);
    body.push_str(&format!(
        "[{} of {} log lines shown; {} offloaded. Full log: hash={}]\n",
        kept.len(),
        n,
        n - kept.len(),
        hash
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
    fn keeps_errors_offloads_noise() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 0;
        cfg.protect_tail_lines = 0;
        let dir = tempfile::tempdir().unwrap();
        let store = CcrStore::new(dir.path(), 3600, 100, 1 << 30, false).unwrap();
        let ctx = CompressCtx {
            query: None,
            config: &cfg,
        };
        let mut lines = vec![];
        for _ in 0..50 {
            lines.push("DEBUG noise".to_string());
        }
        lines.push("ERROR boom".to_string());
        let input = lines.join("\n");
        let out = compress(&input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.compressed.contains("ERROR boom"));
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("offloaded"));
    }
}
