//! Tiny self-contained BM25 over a list of documents (used by query-filtered
//! retrieve and search-result offload). Returns (index, raw_score) desc.

use std::collections::HashSet;

const K1: f32 = 1.5;
const B: f32 = 0.75;

fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

pub fn bm25_rank(query: &str, docs: &[String]) -> Vec<(usize, f32)> {
    let q_terms = tokenize(query);
    if q_terms.is_empty() {
        return vec![];
    }

    let doc_terms: Vec<Vec<String>> = docs.iter().map(|d| tokenize(d)).collect();
    let n = doc_terms.len() as f32;
    let avgdl = if doc_terms.is_empty() {
        1.0
    } else {
        doc_terms.iter().map(|d| d.len() as f32).sum::<f32>() / n
    };

    let mut scores = vec![0.0f32; docs.len()];
    let unique_terms: HashSet<&String> = q_terms.iter().collect();
    for term in unique_terms {
        let df = doc_terms.iter().filter(|d| d.contains(term)).count() as f32;
        if df == 0.0 {
            continue;
        }

        let idf = (((n - df + 0.5) / (df + 0.5)) + 1.0).ln();
        for (i, d) in doc_terms.iter().enumerate() {
            let tf = d.iter().filter(|t| *t == term).count() as f32;
            if tf == 0.0 {
                continue;
            }
            let dl = d.len() as f32;
            scores[i] += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / avgdl));
        }
    }

    let mut ranked: Vec<(usize, f32)> = scores
        .into_iter()
        .enumerate()
        .filter(|(_, s)| *s > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_relevant_doc_first() {
        let docs = vec![
            "the cat sat on the mat".to_string(),
            "database connection error retry".to_string(),
            "weather is nice today".to_string(),
        ];
        let ranked = bm25_rank("database error", &docs);
        assert_eq!(ranked.first().unwrap().0, 1);
    }

    #[test]
    fn empty_query_returns_empty() {
        let docs = vec!["a".to_string()];
        assert!(bm25_rank("", &docs).is_empty());
    }
}
