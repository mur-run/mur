//! Search-result compressor. Reformat: group hits by file. Offload: with a
//! query keep top-K by BM25, else keep the head window; stash the rest.

use crate::bm25::bm25_rank;
use crate::ccr::CcrStore;
use crate::tokenizer::TokenCounter;
use crate::types::{CompressCtx, CompressError, CompressOutput, ContentType};

fn file_of(line: &str) -> &str {
    // grep emits "path:line:content". The path itself may contain a colon
    // (e.g. a Windows drive `C:\src\x.rs`), so we can't just split on the first
    // ':'. Instead find the first ":<digits>:" boundary and treat everything
    // before it as the path.
    let mut start = 0;
    while let Some(rel) = line[start..].find(':') {
        let colon = start + rel;
        let after = &line[colon + 1..];
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 && after[digits..].starts_with(':') {
            return &line[..colon];
        }
        start = colon + 1;
    }
    // No line-number marker found: fall back to the first colon segment.
    line.split(':').next().unwrap_or("")
}

fn group_by_file(items: &[String]) -> String {
    let mut out = String::new();
    let mut current = "";
    for it in items {
        let f = file_of(it);
        if f != current {
            out.push_str(&format!("{f}:\n"));
            current = f;
        }
        // strip the leading "path:" so the file header carries it
        let rest = it
            .strip_prefix(f)
            .and_then(|r| r.strip_prefix(':'))
            .unwrap_or(it);
        out.push_str("  ");
        out.push_str(rest);
        out.push('\n');
    }
    out
}

pub fn compress(
    content: &str,
    ctx: &CompressCtx,
    store: &CcrStore,
    tok: &dyn TokenCounter,
) -> Result<CompressOutput, CompressError> {
    let items: Vec<String> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut transforms = vec!["search.group_by_file".to_string()];
    let cfg = ctx.config;

    let keep_idx: Vec<usize> = if let Some(q) = ctx.query {
        let ranked = bm25_rank(q, &items);
        if ranked.is_empty() {
            (0..items.len().min(cfg.protect_head_lines)).collect()
        } else {
            let mut idx: Vec<usize> = ranked
                .into_iter()
                .take(cfg.retrieve_top_k)
                .map(|(i, _)| i)
                .collect();
            idx.sort_unstable();
            idx
        }
    } else {
        (0..items.len().min(cfg.protect_head_lines)).collect()
    };

    if keep_idx.len() >= items.len() {
        return Ok(CompressOutput {
            compressed: group_by_file(&items),
            hash: None,
            transforms,
        });
    }

    let hash = store
        .put_original(content, items.clone(), ContentType::SearchResults, tok)
        .map_err(|e| CompressError::Store(e.to_string()))?;
    transforms.push("search.offload".to_string());

    let kept: Vec<String> = keep_idx.iter().map(|&i| items[i].clone()).collect();
    let mut body = group_by_file(&kept);
    body.push_str(&format!(
        "[{} of {} hits shown; {} offloaded. Retrieve all with hash={}]\n",
        kept.len(),
        items.len(),
        items.len() - kept.len(),
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

    fn mk(dir: &std::path::Path) -> CcrStore {
        CcrStore::new(dir, 3600, 100, 1 << 30, false).unwrap()
    }

    #[test]
    fn file_of_handles_colons_in_path() {
        assert_eq!(file_of("src/a.rs:10:fn foo"), "src/a.rs");
        assert_eq!(file_of(r"C:\src\a.rs:10:fn foo"), r"C:\src\a.rs");
        assert_eq!(file_of("no-marker-line"), "no-marker-line");
    }

    #[test]
    fn offloads_tail_and_keeps_query_relevant() {
        let mut cfg = CompressConfig::default();
        cfg.protect_head_lines = 1;
        cfg.retrieve_top_k = 1;
        let dir = tempfile::tempdir().unwrap();
        let store = mk(dir.path());
        let ctx = CompressCtx {
            query: Some("database"),
            config: &cfg,
        };
        let input = "a.rs:1:hello world\nb.rs:2:database connection\nc.rs:3:weather";
        let out = compress(input, &ctx, &store, &HeuristicCounter).unwrap();
        assert!(out.hash.is_some());
        assert!(out.compressed.contains("database connection"));
        assert!(out.compressed.contains("hash="));
        // original retrievable
        let got = store.get(out.hash.as_ref().unwrap()).unwrap().unwrap();
        assert_eq!(got.item_count, 3);
    }
}
