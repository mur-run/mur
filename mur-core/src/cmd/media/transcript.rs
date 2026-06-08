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

/// Parse a timestamp like `00:01:02,500` or `00:01:02.500` into milliseconds.
fn parse_ts(s: &str) -> Option<i64> {
    let s = s.trim().replace(',', ".");
    let (hms, ms) = s.split_once('.').unwrap_or((s.as_str(), "0"));
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec) = match parts.as_slice() {
        [h, m, sec] => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?, sec.parse::<i64>().ok()?),
        [m, sec] => (0, m.parse::<i64>().ok()?, sec.parse::<i64>().ok()?),
        _ => return None,
    };
    let millis = format!("{ms:0<3}")[..3].parse::<i64>().ok()?;
    Some(((h * 3600 + m * 60 + sec) * 1000) + millis)
}

/// Parse SubRip (.srt) subtitle text into cues.
pub fn parse_srt(s: &str) -> Vec<Cue> {
    parse_cue_blocks(s)
}

/// Parse WebVTT (.vtt) subtitle text into cues (header + cue identifiers tolerated).
pub fn parse_vtt(s: &str) -> Vec<Cue> {
    parse_cue_blocks(s)
}

/// Shared block parser: split on blank lines, find the `-->` line, join the rest.
fn parse_cue_blocks(s: &str) -> Vec<Cue> {
    let mut out = Vec::new();
    for block in s.split("\n\n") {
        let lines: Vec<&str> = block.lines().map(|l| l.trim_end()).collect();
        let Some(ts_idx) = lines.iter().position(|l| l.contains("-->")) else {
            continue;
        };
        let ts_line = lines[ts_idx];
        let Some((start_s, rest)) = ts_line.split_once("-->") else {
            continue;
        };
        let end_s = rest.split_whitespace().next().unwrap_or("");
        let (Some(start_ms), Some(end_ms)) = (parse_ts(start_s), parse_ts(end_s)) else {
            continue;
        };
        let text = lines[ts_idx + 1..].join(" ").trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push(Cue { start_ms, end_ms, text });
    }
    out
}

#[cfg(test)]
mod subtitle_tests {
    use super::*;

    #[test]
    fn srt_basic() {
        let srt = "1\n00:00:01,000 --> 00:00:04,000\nHello world\n\n2\n00:00:04,000 --> 00:00:06,500\nNext\nline";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0], Cue { start_ms: 1000, end_ms: 4000, text: "Hello world".into() });
        assert_eq!(cues[1], Cue { start_ms: 4000, end_ms: 6500, text: "Next line".into() });
    }

    #[test]
    fn vtt_with_header_and_dot_millis() {
        let vtt = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nHi there";
        let cues = parse_vtt(vtt);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0], Cue { start_ms: 1000, end_ms: 3000, text: "Hi there".into() });
    }

    #[test]
    fn parse_ts_forms() {
        assert_eq!(parse_ts("00:01:02,500"), Some(62500));
        assert_eq!(parse_ts("01:02.250"), Some(62250));
        assert_eq!(parse_ts("garbage"), None);
    }
}
