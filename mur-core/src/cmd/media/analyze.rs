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
    #[serde(default, deserialize_with = "de_seconds_to_ms")]
    pub t_ms: i64,
}

/// Deserialize a model-provided timestamp into milliseconds.
///
/// The reduce prompt feeds the model second-granularity markers (`[t=Ns]`), so the
/// bundled 2B model returns timestamps in **seconds** — and frequently in loose shapes:
/// a bare int/float, a numeric string, or a `[start, end]` array. A plain `i64` field
/// rejected the array/float/string forms, so the whole structured parse failed and fell
/// back to dumping raw JSON. We accept any of these, take the first numeric value, treat
/// it as seconds, and store milliseconds so `deep_link` (which divides by 1000) renders
/// the correct `?t=Ns` deep link instead of collapsing to `t=0`.
fn de_seconds_to_ms<'de, D>(de: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = serde_json::Value::deserialize(de)?;
    let secs = first_number(&v).unwrap_or(0.0).max(0.0);
    Ok((secs * 1000.0) as i64)
}

/// First numeric value reachable in `v`: a number, a numeric string, or the first
/// numeric element of an array (recursively). `None` for anything else.
fn first_number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        serde_json::Value::Array(a) => a.iter().find_map(first_number),
        _ => None,
    }
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
        assert_eq!(
            deep_link("https://youtu.be/abc", 65_000),
            "https://youtu.be/abc?t=65s"
        );
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
            out.push_str(&format!(
                "- {} （{}）\n",
                kp.text,
                deep_link(source, kp.t_ms)
            ));
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
    fn t_ms_accepts_seconds_in_loose_shapes() {
        // Bare integer seconds → milliseconds.
        let r = parse_analysis(r#"{"key_points":[{"text":"a","t_ms":5}]}"#);
        assert_eq!(r.key_points[0].t_ms, 5_000);
        // [start, end] array → first element, as seconds.
        let r = parse_analysis(r#"{"key_points":[{"text":"a","t_ms":[7,9]}]}"#);
        assert_eq!(r.key_points[0].t_ms, 7_000);
        // Numeric string seconds.
        let r = parse_analysis(r#"{"key_points":[{"text":"a","t_ms":"12"}]}"#);
        assert_eq!(r.key_points[0].t_ms, 12_000);
        // Float seconds.
        let r = parse_analysis(r#"{"key_points":[{"text":"a","t_ms":3.5}]}"#);
        assert_eq!(r.key_points[0].t_ms, 3_500);
    }

    #[test]
    fn t_ms_array_no_longer_breaks_structured_parse() {
        // Regression: the bundled model emitted t_ms as [start,end]; the old i64 field
        // rejected it, so the whole structured parse failed and dumped raw JSON.
        let json = r#"{"topic":"T","key_points":[{"text":"p","t_ms":[0,0]}],"conclusion":"c"}"#;
        let r = parse_analysis(json);
        assert_eq!(r.topic, "T");
        assert_eq!(r.key_points[0].text, "p");
        assert_eq!(r.conclusion, "c");
    }

    #[test]
    fn t_ms_seconds_render_to_correct_deep_link() {
        // 5 seconds from the model → ?t=5s (not t=0).
        let r = parse_analysis(r#"{"key_points":[{"text":"p","t_ms":5}]}"#);
        let md = render_markdown(&r, "https://youtu.be/abc");
        assert!(md.contains("https://youtu.be/abc?t=5s"), "got: {md}");
    }

    #[test]
    fn render_has_links() {
        let r = AnalysisResult {
            topic: "Topic".into(),
            key_points: vec![KeyPoint {
                text: "first".into(),
                t_ms: 65_000,
            }],
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

use super::error::MediaError;
use super::transcript::{self, Chunk};
use mur_common::config::DEFAULT_BUNDLED_MODEL_ID;
use serde_json::json;
use std::time::Duration;

/// Per-chunk "map" instruction: extract key points with approximate timestamps.
fn map_system() -> &'static str {
    "你是影片分析助手。針對這段字幕，列出 3-5 個重點，每點儘量附上時間（秒）。只回重點，不要客套。"
}

/// Final "reduce" instruction: emit STRICT JSON matching AnalysisResult.
fn reduce_system(mode: AnalysisMode) -> &'static str {
    match mode {
        AnalysisMode::Conclusions => {
            "你是影片分析助手。根據以下各段重點，輸出嚴格 JSON：{\"topic\":..,\"key_points\":[{\"text\":..,\"t_ms\":..}],\"key_moments\":[{\"text\":..,\"t_ms\":..}],\"conclusion\":\"深入的分析與結論\"}。t_ms 填該重點的時間，用「秒」的單一整數（例如 12），不要用陣列或區間。只輸出 JSON，用繁體中文。"
        }
        _ => {
            "你是影片分析助手。根據以下各段重點，輸出嚴格 JSON：{\"topic\":..,\"key_points\":[{\"text\":..,\"t_ms\":..}],\"key_moments\":[{\"text\":..,\"t_ms\":..}],\"conclusion\":\"摘要\"}。t_ms 填該重點的時間，用「秒」的單一整數（例如 12），不要用陣列或區間。只輸出 JSON，用繁體中文。"
        }
    }
}

/// Build an OpenAI-compatible chat request (text-only, temperature 0 for determinism).
///
/// `chat_template_kwargs.enable_thinking=false` disables the bundled Qwen3 model's
/// "thinking" mode. Reasoning models otherwise spend the entire `max_tokens` budget
/// emitting chain-of-thought into `message.reasoning` and never populate
/// `message.content`, which `parse_completion` reads — yielding empty output. The
/// kwarg is ignored by chat templates that don't reference it, so it is harmless for
/// non-reasoning models.
fn chat_request(system: &str, user: &str) -> serde_json::Value {
    json!({
        "model": DEFAULT_BUNDLED_MODEL_ID,
        "temperature": 0,
        "chat_template_kwargs": { "enable_thinking": false },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "max_tokens": 1024
    })
}

async fn call_model(body: serde_json::Value) -> Result<String, MediaError> {
    let client = super::shared_client();
    let base = super::local_base_url().map_err(|_| MediaError::ModelOffline)?;
    let resp: serde_json::Value = client
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .json(&body)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|_| MediaError::ModelOffline)?
        .json()
        .await
        .map_err(|_| MediaError::ModelOffline)?;
    super::scene::parse_completion(&resp).ok_or(MediaError::ModelOffline)
}

/// Analyze a video. `source = None` ⇒ use the last-opened source (spec §4.2/§5).
#[allow(dead_code)]
pub async fn analyze(
    source: Option<&str>,
    mode: Option<&str>,
    _focus: Option<&str>,
) -> Result<String, MediaError> {
    let mode = AnalysisMode::parse(mode);
    let home = crate::cmd::resolve_mur_home().map_err(|_| MediaError::SourceUnresolvable)?;
    let source = match source {
        Some(s) => s.to_string(),
        None => super::resolve::load_last_source(&home).ok_or(MediaError::SourceUnresolvable)?,
    };
    if super::resolve::is_drm_host(&source) {
        return Err(MediaError::DrmProtected);
    }
    let tr = transcript::get(&home, &source)?;
    let chunks: Vec<Chunk> = tr.chunks_for_analysis();
    if chunks.is_empty() {
        return Err(MediaError::NoTranscript);
    }

    // Map: summarize each chunk (sequential — local model, small machine).
    let mut summaries = String::new();
    for c in &chunks {
        let secs = c.start_ms / 1000;
        let user = format!("[t={secs}s]\n{}", c.text);
        let s = call_model(chat_request(map_system(), &user)).await?;
        summaries.push_str(&format!("[t={secs}s] {}\n", s.trim()));
    }

    // Reduce: produce structured JSON, parse, render.
    let raw = call_model(chat_request(reduce_system(mode), &summaries)).await?;
    let result = parse_analysis(&raw);
    Ok(render_markdown(&result, &source))
}
