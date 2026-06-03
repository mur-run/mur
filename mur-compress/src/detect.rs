//! Deterministic content-type detection (regex heuristics, no ML).

use std::sync::LazyLock;

use regex::Regex;

use crate::config::CompressConfig;
use crate::types::ContentType;

static SEARCH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^\s:]+:\d+:").unwrap());
static LOG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(ERROR|WARN|WARNING|INFO|DEBUG|TRACE|FATAL)\b").unwrap());

pub fn detect_content_type(content: &str, cfg: &CompressConfig) -> ContentType {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return ContentType::Generic;
    }

    // JSON: must actually parse.
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
    {
        return ContentType::Json;
    }

    // Git diff: structural markers.
    if content.contains("diff --git")
        || content
            .lines()
            .any(|l| l.starts_with("@@ ") || l.starts_with("@@-"))
        || (content.contains("\n--- ") && content.contains("\n+++ "))
    {
        return ContentType::GitDiff;
    }

    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return ContentType::Generic;
    }
    let n = lines.len() as f32;

    let search_hits = lines.iter().filter(|l| SEARCH_RE.is_match(l)).count() as f32;
    if search_hits / n >= cfg.detect.search_min_ratio {
        return ContentType::SearchResults;
    }

    let log_hits = lines.iter().filter(|l| LOG_RE.is_match(l)).count() as f32;
    if log_hits / n >= cfg.detect.log_min_ratio {
        return ContentType::BuildLog;
    }

    ContentType::Generic
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CompressConfig {
        CompressConfig::default()
    }

    #[test]
    fn detects_json_array() {
        assert_eq!(
            detect_content_type(r#"[{"a":1},{"a":2}]"#, &cfg()),
            ContentType::Json
        );
    }

    #[test]
    fn detects_search_results() {
        let s = "src/a.rs:10:fn foo\nsrc/b.rs:22:let x\nsrc/c.rs:3:use bar";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::SearchResults);
    }

    #[test]
    fn detects_git_diff() {
        let s = "diff --git a/x b/x\n@@ -1,2 +1,2 @@\n-old\n+new";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::GitDiff);
    }

    #[test]
    fn detects_build_log() {
        let s = "INFO starting\nERROR boom\nWARN careful\nDEBUG trace";
        assert_eq!(detect_content_type(s, &cfg()), ContentType::BuildLog);
    }

    #[test]
    fn falls_back_to_generic() {
        assert_eq!(
            detect_content_type("just some prose here", &cfg()),
            ContentType::Generic
        );
    }
}
