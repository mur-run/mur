//! Heuristic extractive compression for Phase 3.4 `mur ask` (§4 of spec).
//!
//! Sentence-level position + jaccard-overlap scoring. Pure function, no ML,
//! no I/O, deterministic. Called from `prompt::render` as Stage 1 of the
//! overflow loop — only fires when the full prompt exceeds
//! `max_context_tokens` AND `AskConfig.compress_hits_enabled` is true.
#![allow(dead_code)] // wired by Task 3 (prompt::render integration).

use super::retrieve::ResolvedHit;
use std::collections::HashSet;

/// Hit must have ≥ this many sentences to be eligible for compression.
pub(crate) const COMPRESS_MIN_SENTENCES: usize = 4;

/// Hit must have ≥ this many chars to be eligible for compression.
pub(crate) const COMPRESS_MIN_CHARS: usize = 400;

/// Weight of the position signal in the scoring formula.
pub(crate) const POSITION_WEIGHT: f64 = 0.7;

/// Weight of the query-overlap (jaccard) signal.
pub(crate) const JACCARD_WEIGHT: f64 = 0.3;

/// Citation-invariant floor: every hit emits ≥ this many sentences.
pub(crate) const MIN_SENTENCES_PER_HIT: usize = 1;

/// Small English stopword list. Hardcoded (no crate dependency).
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "did", "do", "for", "had", "has", "have", "i",
    "in", "is", "it", "not", "of", "on", "or", "that", "the", "these", "this", "those", "to",
    "was", "were", "with", "you",
];

/// Byte-walking sentence splitter. Breaks on `". "`, `"! "`, `"? "`,
/// `"\n\n"`. Does NOT handle abbreviations (`Dr. Smith` splits — acceptable
/// for conversational data). Returns non-empty sentences with terminators
/// preserved so joined output reads naturally.
pub(crate) fn split_sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        let is_terminator =
            (c == b'.' || c == b'!' || c == b'?') && i + 1 < bytes.len() && bytes[i + 1] == b' ';
        let is_para_break = c == b'\n' && i + 1 < bytes.len() && bytes[i + 1] == b'\n';
        if is_terminator {
            let end = i + 1; // include terminator
            let seg = s[start..end].trim();
            if !seg.is_empty() {
                out.push(seg);
            }
            start = i + 2; // skip the space
            i = start;
            continue;
        }
        if is_para_break {
            let end = i;
            let seg = s[start..end].trim();
            if !seg.is_empty() {
                out.push(seg);
            }
            start = i + 2;
            i = start;
            continue;
        }
        i += 1;
    }
    if start < bytes.len() {
        let seg = s[start..].trim();
        if !seg.is_empty() {
            out.push(seg);
        }
    }
    out
}

/// Lowercase + strip punctuation + split on whitespace + drop stopwords.
/// Returns a `HashSet<String>` so `jaccard_overlap` can use set ops.
pub(crate) fn tokenize_query(q: &str) -> HashSet<String> {
    tokenize_to_set(q)
}

fn tokenize_to_set(s: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for raw in s.split(|c: char| !c.is_alphanumeric()) {
        let tok = raw.to_ascii_lowercase();
        if tok.is_empty() {
            continue;
        }
        if STOPWORDS.iter().any(|sw| *sw == tok) {
            continue;
        }
        out.insert(tok);
    }
    out
}

/// `|S ∩ Q| / |S ∪ Q|`, or 0.0 if either set is empty.
pub(crate) fn jaccard_overlap(sentence: &str, query_tokens: &HashSet<String>) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }
    let s = tokenize_to_set(sentence);
    if s.is_empty() {
        return 0.0;
    }
    let intersection = s.intersection(query_tokens).count();
    let union = s.union(query_tokens).count();
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// 1.0 for the first sentence (topic), 0.8 for the last (conclusion, only if
/// N ≥ 3), 0.5 for everything else.
pub(crate) fn position_weight(i: usize, total: usize) -> f64 {
    if i == 0 {
        1.0
    } else if total >= 3 && i == total - 1 {
        0.8
    } else {
        0.5
    }
}

/// Final sentence score: `0.7 × position + 0.3 × jaccard`.
pub(crate) fn score_sentence(
    sentence: &str,
    index: usize,
    total: usize,
    query_tokens: &HashSet<String>,
) -> f64 {
    POSITION_WEIGHT * position_weight(index, total)
        + JACCARD_WEIGHT * jaccard_overlap(sentence, query_tokens)
}

/// Compress each hit's snippet to its top-scoring sentences (Phase 3.4).
/// Preserves hit ordering and citation-anchor metadata; only `snippet`
/// changes.
///
/// SKIP rule: hits with `< COMPRESS_MIN_SENTENCES` OR `< COMPRESS_MIN_CHARS`
/// pass through unchanged — protects layer=2 span hits and short summaries.
///
/// Floor: eligible hits still emit ≥ `MIN_SENTENCES_PER_HIT` sentence even
/// if `target_chars_per_hit` is 0 — citation anchors can't vanish.
pub fn compress_hits(
    hits: Vec<ResolvedHit>,
    query: &str,
    target_chars_per_hit: usize,
) -> Vec<ResolvedHit> {
    let query_tokens = tokenize_query(query);
    hits.into_iter()
        .map(|h| compress_one(h, &query_tokens, target_chars_per_hit))
        .collect()
}

fn compress_one(
    h: ResolvedHit,
    query_tokens: &HashSet<String>,
    target_chars_per_hit: usize,
) -> ResolvedHit {
    let sentences = split_sentences(&h.snippet);
    // SKIP: too few sentences OR too short to be worth compressing
    if sentences.len() < COMPRESS_MIN_SENTENCES || h.snippet.len() < COMPRESS_MIN_CHARS {
        return h;
    }
    let total = sentences.len();
    let scored: Vec<(usize, f64)> = sentences
        .iter()
        .enumerate()
        .map(|(i, s)| (i, score_sentence(s, i, total, query_tokens)))
        .collect();
    let kept_indices = pick_by_score(&scored, &sentences, target_chars_per_hit);
    let mut sorted = kept_indices;
    sorted.sort();
    let new_snippet = sorted
        .iter()
        .map(|&i| sentences[i])
        .collect::<Vec<_>>()
        .join(" ");
    ResolvedHit {
        snippet: new_snippet,
        ..h
    }
}

/// Greedy top-K-by-score, bounded by `target_chars_per_hit`. Always emits
/// at least `MIN_SENTENCES_PER_HIT` sentences (picks the top-scorer(s)
/// even if target_chars is 0).
fn pick_by_score(
    scored: &[(usize, f64)],
    sentences: &[&str],
    target_chars_per_hit: usize,
) -> Vec<usize> {
    let mut ranked: Vec<&(usize, f64)> = scored.iter().collect();
    // Sort by score descending; stable tie-break on index (ascending).
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut kept = Vec::new();
    let mut chars = 0usize;
    for (i, _score) in ranked {
        let sl = sentences[*i].len();
        // Always keep the first MIN_SENTENCES_PER_HIT highest-scored items
        // even if they'd exceed target_chars (floor invariant).
        if kept.len() < MIN_SENTENCES_PER_HIT {
            kept.push(*i);
            chars += sl;
            continue;
        }
        if chars + sl + 1 /* join space */ > target_chars_per_hit {
            break;
        }
        kept.push(*i);
        chars += sl + 1;
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::ask::HitInfo;

    fn hit(snippet: &str) -> ResolvedHit {
        ResolvedHit {
            layer: 0,
            info: HitInfo {
                layer: 0,
                source: "cc".into(),
                conv_id: "c1".into(),
                date: chrono::NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                score: 0.9,
            },
            snippet: snippet.into(),
            line_hint: Some(1),
            span_index_in_summary: None,
            vector: None,
        }
    }

    #[test]
    fn split_sentences_basic() {
        let out = split_sentences("A. B! C?");
        assert_eq!(out.len(), 3);
        assert!(out[0].contains('A'));
        assert!(out[1].contains('B'));
        assert!(out[2].contains('C'));
    }

    #[test]
    fn jaccard_overlap_empty_query_is_zero() {
        let query_tokens = tokenize_query("");
        let s = "any text here";
        assert_eq!(jaccard_overlap(s, &query_tokens), 0.0);
    }

    #[test]
    fn position_weight_is_exact_constants() {
        // N=5 → first=1.0, last=0.8, middle=0.5
        assert!((position_weight(0, 5) - 1.0).abs() < 1e-9);
        assert!((position_weight(4, 5) - 0.8).abs() < 1e-9);
        assert!((position_weight(2, 5) - 0.5).abs() < 1e-9);
        // N=2 → last bonus disabled; index 1 is middle
        assert!((position_weight(0, 2) - 1.0).abs() < 1e-9);
        assert!((position_weight(1, 2) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn compress_hits_skips_short_hits() {
        // Hit with 2 sentences — below COMPRESS_MIN_SENTENCES (4) → pass through.
        let h = hit("Short hit. Just two sentences.");
        let out = compress_hits(vec![h.clone()], "query", 10);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].snippet, h.snippet);
    }

    #[test]
    fn compress_hits_keeps_at_least_one_sentence() {
        // Construct a hit that qualifies for compression (6 sentences, >400 chars)
        // with target_chars=0 to force the floor to kick in.
        let long = (0..10)
            .map(|i| format!("Sentence number {i} goes here with enough filler content that makes it long enough."))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(long.len() >= 400);
        let h = hit(&long);
        let out = compress_hits(vec![h], "query", 0);
        assert_eq!(out.len(), 1);
        // Floor invariant: at least 1 non-empty sentence survives.
        assert!(!out[0].snippet.is_empty());
    }

    #[test]
    fn compress_hits_preserves_citation_metadata() {
        // Force compression by making the hit long enough to be eligible.
        let long_snippet = (0..12)
            .map(|i| format!("Info sentence number {i} with extended body text content."))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(long_snippet.len() >= 400);
        let mut h = hit(&long_snippet);
        h.layer = 2;
        h.line_hint = Some(42);
        h.span_index_in_summary = Some(7);
        h.vector = Some(vec![0.1; 16]);
        let original_info = h.info.clone();
        let out = compress_hits(vec![h], "info", 150);
        assert_eq!(out.len(), 1);
        let o = &out[0];
        // Metadata unchanged
        assert_eq!(o.layer, 2);
        assert_eq!(o.info.source, original_info.source);
        assert_eq!(o.info.conv_id, original_info.conv_id);
        assert_eq!(o.line_hint, Some(42));
        assert_eq!(o.span_index_in_summary, Some(7));
        assert_eq!(o.vector, Some(vec![0.1; 16]));
        // Snippet actually compressed (shorter than original)
        assert!(o.snippet.len() < long_snippet.len());
        assert!(!o.snippet.is_empty());
    }
}
