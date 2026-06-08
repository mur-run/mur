//! `video_analyze`: transcript → chunk → map → reduce → structured markdown.

use serde::{Deserialize, Serialize};

/// What kind of analysis to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisMode {
    Summary,
    Conclusions,
    Qa,
}

impl AnalysisMode {
    /// Parse a mode string; unknown/empty ⇒ Summary.
    pub fn parse(s: Option<&str>) -> AnalysisMode {
        match s.unwrap_or("summary").trim().to_ascii_lowercase().as_str() {
            "conclusions" | "conclusion" | "分析" | "結論" => AnalysisMode::Conclusions,
            "qa" | "q&a" | "question" => AnalysisMode::Qa,
            _ => AnalysisMode::Summary,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct KeyPoint {
    pub text: String,
    #[serde(default)]
    pub t_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AnalysisResult {
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub key_points: Vec<KeyPoint>,
    #[serde(default)]
    pub key_moments: Vec<KeyPoint>,
    #[serde(default)]
    pub conclusion: String,
}

/// Build a clickable reference for a timestamp.
/// YouTube ⇒ `<url>?t=Ns` / `<url>&t=Ns`; otherwise ⇒ `[hh:mm:ss]` / `[mm:ss]`.
pub fn deep_link(source: &str, t_ms: i64) -> String {
    let secs = (t_ms / 1000).max(0);
    let lower = source.to_ascii_lowercase();
    if lower.contains("youtube.com") || lower.contains("youtu.be") {
        let sep = if source.contains('?') { '&' } else { '?' };
        return format!("{source}{sep}t={secs}s");
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("[{h:02}:{m:02}:{s:02}]")
    } else {
        format!("[{m:02}:{s:02}]")
    }
}

#[cfg(test)]
mod basics_tests {
    use super::*;

    #[test]
    fn mode_parse() {
        assert_eq!(AnalysisMode::parse(None), AnalysisMode::Summary);
        assert_eq!(AnalysisMode::parse(Some("Summary")), AnalysisMode::Summary);
        assert_eq!(AnalysisMode::parse(Some("結論")), AnalysisMode::Conclusions);
        assert_eq!(AnalysisMode::parse(Some("qa")), AnalysisMode::Qa);
        assert_eq!(AnalysisMode::parse(Some("weird")), AnalysisMode::Summary);
    }

    #[test]
    fn deep_link_youtube_and_local() {
        assert_eq!(deep_link("https://youtu.be/abc", 65_000), "https://youtu.be/abc?t=65s");
        assert_eq!(
            deep_link("https://www.youtube.com/watch?v=abc", 65_000),
            "https://www.youtube.com/watch?v=abc&t=65s"
        );
        assert_eq!(deep_link("/movies/x.mkv", 65_000), "[01:05]");
        assert_eq!(deep_link("/movies/x.mkv", 3_725_000), "[01:02:05]");
    }
}
