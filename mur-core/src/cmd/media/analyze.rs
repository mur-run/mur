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

/// Parse the reducer's JSON into a structured result. On invalid JSON, wrap the raw
/// text as the conclusion so the user still gets something useful.
pub fn parse_analysis(raw: &str) -> AnalysisResult {
    // The model may wrap JSON in a ```json fence; strip a leading/trailing fence.
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|s| s.trim_end_matches("```").trim())
        .unwrap_or(trimmed);
    match serde_json::from_str::<AnalysisResult>(body) {
        Ok(r) => r,
        Err(_) => AnalysisResult {
            topic: String::new(),
            key_points: Vec::new(),
            key_moments: Vec::new(),
            conclusion: raw.trim().to_string(),
        },
    }
}

/// Render a result as markdown, with clickable timestamps for each point/moment.
pub fn render_markdown(result: &AnalysisResult, source: &str) -> String {
    let mut out = String::new();
    if !result.topic.is_empty() {
        out.push_str(&format!("## {}\n\n", result.topic));
    }
    if !result.key_points.is_empty() {
        out.push_str("### 重點\n");
        for kp in &result.key_points {
            out.push_str(&format!("- {} （{}）\n", kp.text, deep_link(source, kp.t_ms)));
        }
        out.push('\n');
    }
    if !result.key_moments.is_empty() {
        out.push_str("### 關鍵時刻\n");
        for m in &result.key_moments {
            out.push_str(&format!("- {} （{}）\n", m.text, deep_link(source, m.t_ms)));
        }
        out.push('\n');
    }
    if !result.conclusion.is_empty() {
        out.push_str(&format!("### 結論\n{}\n", result.conclusion));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod render_tests {
    use super::*;

    #[test]
    fn parse_valid_and_fenced_json() {
        let json = r#"{"topic":"T","key_points":[{"text":"p","t_ms":1000}],"conclusion":"c"}"#;
        let r = parse_analysis(json);
        assert_eq!(r.topic, "T");
        assert_eq!(r.key_points[0].text, "p");
        let fenced = format!("```json\n{json}\n```");
        assert_eq!(parse_analysis(&fenced).topic, "T");
    }

    #[test]
    fn parse_invalid_falls_back_to_conclusion() {
        let r = parse_analysis("not json at all");
        assert!(r.topic.is_empty());
        assert_eq!(r.conclusion, "not json at all");
    }

    #[test]
    fn render_has_links() {
        let r = AnalysisResult {
            topic: "Topic".into(),
            key_points: vec![KeyPoint { text: "first".into(), t_ms: 65_000 }],
            key_moments: vec![],
            conclusion: "done".into(),
        };
        let md = render_markdown(&r, "https://youtu.be/abc");
        assert!(md.contains("## Topic"));
        assert!(md.contains("first"));
        assert!(md.contains("https://youtu.be/abc?t=65s"));
        assert!(md.contains("### 結論"));
    }
}
