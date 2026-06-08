//! Transcript acquisition + pure subtitle parsing / chunking.
//!
//! Captions-first: YouTube via yt-dlp json3; local files via sidecar .srt/.vtt or an
//! embedded track (ffmpeg). No Whisper fallback in v2 (deferred — spec §7).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub start_ms: i64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub source_id: String,
    pub lang: String,
    pub cues: Vec<Cue>,
    pub chapters: Vec<Chapter>,
}

/// A coarse time-window of concatenated cue text, used as one map-reduce unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Parse yt-dlp `json3` subtitle output into cues.
///
/// json3 shape: `{ "events": [ { "tStartMs", "dDurationMs", "segs": [ {"utf8"} ] } ] }`.
/// Events without `segs` (style/window defs) or whose joined text is blank are skipped.
pub fn parse_json3(json: &str) -> Vec<Cue> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Some(events) = v.get("events").and_then(|e| e.as_array()) else {
        return out;
    };
    for ev in events {
        let Some(segs) = ev.get("segs").and_then(|s| s.as_array()) else {
            continue;
        };
        let text: String = segs
            .iter()
            .filter_map(|s| s.get("utf8").and_then(|u| u.as_str()))
            .collect();
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start_ms = ev.get("tStartMs").and_then(|t| t.as_i64()).unwrap_or(0);
        let dur = ev.get("dDurationMs").and_then(|d| d.as_i64()).unwrap_or(0);
        out.push(Cue {
            start_ms,
            end_ms: start_ms + dur,
            text,
        });
    }
    out
}

#[cfg(test)]
mod json3_tests {
    use super::*;

    #[test]
    fn parses_events_and_skips_blank() {
        let json = r#"{"events":[
            {"tStartMs":0,"dDurationMs":1200,"segs":[{"utf8":"Hello"},{"utf8":" world"}]},
            {"tStartMs":2000,"dDurationMs":500,"segs":[{"utf8":"\n"}]},
            {"tStartMs":3000,"dDurationMs":800,"segs":[{"utf8":"Next"}]}
        ]}"#;
        let cues = parse_json3(json);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start_ms: 0, end_ms: 1200, text: "Hello world".into() });
        assert_eq!(cues[1], Cue { start_ms: 3000, end_ms: 3800, text: "Next".into() });
    }

    #[test]
    fn malformed_json_is_empty() {
        assert!(parse_json3("not json").is_empty());
        assert!(parse_json3("{}").is_empty());
    }
}
