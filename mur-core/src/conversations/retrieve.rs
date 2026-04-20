//! Retrieval — Mode A (timeline) and Mode B (search).
//! Mode C (NL Q&A) is Phase 2.
#![allow(dead_code)] // Phase 1: search/show_summary wired in by later CLI tasks.

use std::cmp::Reverse;

use anyhow::Result;
use chrono::NaiveDate;
use mur_common::{Message, Source};

use super::paths::summary_paths_for;
use super::store::{list_raw_dirs, read_day};

#[derive(Debug, Clone)]
pub struct DaySummary {
    pub date: NaiveDate,
    pub msg_count: usize,
    pub sources: Vec<Source>,
    pub summary_exists: bool,
}

/// Mode A — list days (Layer 1 progressive disclosure).
pub fn list_days(
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    sources_filter: &[Source],
    root_override: Option<&str>,
) -> Result<Vec<DaySummary>> {
    let mut out = Vec::new();
    for (date, _dir) in list_raw_dirs(root_override)? {
        if let Some(s) = since
            && date < s
        {
            continue;
        }
        if let Some(u) = until
            && date > u
        {
            continue;
        }
        let msgs = read_day(date, root_override)?;
        if msgs.is_empty() {
            continue;
        }
        let sources: Vec<Source> = {
            let mut set: std::collections::BTreeSet<Source> = msgs.iter().map(|m| m.src).collect();
            if !sources_filter.is_empty() {
                set.retain(|s| sources_filter.contains(s));
                if set.is_empty() {
                    continue;
                }
            }
            set.into_iter().collect()
        };
        let (md, _) = summary_paths_for(date, root_override);
        out.push(DaySummary {
            date,
            msg_count: msgs.len(),
            sources,
            summary_exists: md.exists(),
        });
    }
    out.sort_by_key(|s| Reverse(s.date));
    Ok(out)
}

/// Mode A — show all messages for a day (Layer 2 without summary, Layer 3 raw).
pub fn show_day(date: NaiveDate, root_override: Option<&str>) -> Result<Vec<Message>> {
    read_day(date, root_override)
}

/// Mode A — show the rendered summary file for a day.
pub fn show_summary(date: NaiveDate, root_override: Option<&str>) -> Result<Option<String>> {
    let (md, _) = summary_paths_for(date, root_override);
    if !md.exists() {
        return Ok(None);
    }
    Ok(Some(std::fs::read_to_string(md)?))
}

/// Mode B — semantic search via LanceDB + keyword rerank (0.7/0.3).
pub async fn search(
    query: &str,
    embedding: Vec<f32>,
    limit: usize,
    source_filter: Option<Source>,
    root_override: Option<&str>,
) -> Result<Vec<SearchResult>> {
    let idx = super::index::ConversationIndex::open(embedding.len() as i32, root_override).await?;
    let vec_hits = idx
        .search(&embedding, limit * 3, source_filter, None)
        .await?;

    let q_lower = query.to_lowercase();
    let q_words: Vec<&str> = q_lower.split_whitespace().collect();

    let mut out: Vec<SearchResult> = vec_hits
        .into_iter()
        .map(|h| {
            let vec_score = 1.0 / (1.0 + h.distance as f64);
            let kw_hits = q_words
                .iter()
                .filter(|w| h.content.to_lowercase().contains(*w))
                .count() as f64;
            let kw_score = if q_words.is_empty() {
                0.0
            } else {
                kw_hits / q_words.len() as f64
            };
            let combined = 0.7 * vec_score + 0.3 * kw_score;
            SearchResult {
                id: h.id,
                ts: h.ts,
                source: h.source,
                conv_id: h.conv_id,
                snippet: h.content,
                score: combined,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub ts: i64,
    pub source: Source,
    pub conv_id: String,
    pub snippet: String,
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use mur_common::{Content, Message, Role, Source};

    fn append(root: &str, ts: (i32, u32, u32, u32), src: Source, text: &str) {
        let t = chrono::Utc
            .with_ymd_and_hms(ts.0, ts.1, ts.2, ts.3, 0, 0)
            .unwrap();
        let m = Message {
            v: 1,
            ts: t,
            src,
            conv: "c".into(),
            role: Role::User,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        };
        crate::conversations::store::append(&m, Some(root)).unwrap();
    }

    #[test]
    fn mode_a_timeline_lists_days() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        append(root, (2026, 4, 18, 10), Source::ClaudeCode, "yesterday");
        append(root, (2026, 4, 19, 10), Source::Cursor, "today");
        let days = list_days(None, None, &[], Some(root)).unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(
            days[0].date,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap()
        );
    }

    #[test]
    fn mode_a_show_day_returns_all_messages_for_date() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        append(root, (2026, 4, 19, 9), Source::ClaudeCode, "hello");
        append(root, (2026, 4, 19, 10), Source::Slack, "world");
        let d = chrono::NaiveDate::from_ymd_opt(2026, 4, 19).unwrap();
        let msgs = show_day(d, Some(root)).unwrap();
        assert_eq!(msgs.len(), 2);
    }
}
