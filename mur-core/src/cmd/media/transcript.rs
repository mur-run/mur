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

/// Default analysis chunk window / overlap (seconds). Spec §2: 60s windows w/ overlap.
pub const DEFAULT_CHUNK_WINDOW_SECS: i64 = 60;
pub const DEFAULT_CHUNK_OVERLAP_SECS: i64 = 10;

impl Transcript {
    /// Concatenated cue text within ±`span_ms` of `t_ms` (for live "what did he say").
    pub fn window(&self, t_ms: i64, span_ms: i64) -> String {
        let lo = t_ms - span_ms;
        let hi = t_ms + span_ms;
        self.cues
            .iter()
            .filter(|c| c.end_ms >= lo && c.start_ms <= hi)
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Fixed time-window chunks with overlap, timestamps preserved.
    /// A cue belongs to a window if its `start_ms` is within `[win_start, win_end)`.
    pub fn chunks(&self, window_secs: i64, overlap_secs: i64) -> Vec<Chunk> {
        let window_ms = window_secs.max(1) * 1000;
        let step_ms = (window_secs - overlap_secs).max(1) * 1000;
        let last_end = self.cues.iter().map(|c| c.end_ms).max().unwrap_or(0);
        let mut out = Vec::new();
        let mut win_start = 0i64;
        while win_start <= last_end {
            let win_end = win_start + window_ms;
            let text = self
                .cues
                .iter()
                .filter(|c| c.start_ms >= win_start && c.start_ms < win_end)
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !text.is_empty() {
                out.push(Chunk { start_ms: win_start, end_ms: win_end, text });
            }
            win_start += step_ms;
        }
        out
    }

    /// Chapter-aligned chunks when chapters exist, else `chunks()` with defaults.
    pub fn chunks_for_analysis(&self) -> Vec<Chunk> {
        if self.chapters.is_empty() {
            return self.chunks(DEFAULT_CHUNK_WINDOW_SECS, DEFAULT_CHUNK_OVERLAP_SECS);
        }
        let mut out = Vec::new();
        for (i, ch) in self.chapters.iter().enumerate() {
            let start = ch.start_ms;
            let end = self
                .chapters
                .get(i + 1)
                .map(|n| n.start_ms)
                .unwrap_or(i64::MAX);
            let text = self
                .cues
                .iter()
                .filter(|c| c.start_ms >= start && c.start_ms < end)
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !text.is_empty() {
                out.push(Chunk { start_ms: start, end_ms: end.min(self.last_end()), text });
            }
        }
        out
    }

    fn last_end(&self) -> i64 {
        self.cues.iter().map(|c| c.end_ms).max().unwrap_or(0)
    }
}

// ── Cache ──

use std::path::{Path, PathBuf};

/// Deterministic cache path for a source's transcript.
pub fn cache_path(mur_home: &Path, source: &str) -> PathBuf {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    mur_home
        .join("runtime")
        .join("transcripts")
        .join(format!("{hex}.json"))
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn cache_path_is_stable_and_unique() {
        let home = Path::new("/tmp/murhome");
        let a = cache_path(home, "https://youtu.be/abc");
        let a2 = cache_path(home, "https://youtu.be/abc");
        let b = cache_path(home, "https://youtu.be/xyz");
        assert_eq!(a, a2);
        assert_ne!(a, b);
        assert!(a.to_string_lossy().ends_with(".json"));
        assert!(a.starts_with("/tmp/murhome/runtime/transcripts"));
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    fn t(cues: Vec<Cue>, chapters: Vec<Chapter>) -> Transcript {
        Transcript { source_id: "x".into(), lang: "en".into(), cues, chapters }
    }

    fn cue(start_ms: i64, text: &str) -> Cue {
        Cue { start_ms, end_ms: start_ms + 1000, text: text.into() }
    }

    #[test]
    fn window_picks_nearby() {
        let tr = t(vec![cue(0, "a"), cue(5000, "b"), cue(10000, "c")], vec![]);
        assert_eq!(tr.window(5000, 1500), "b");
        assert_eq!(tr.window(5000, 6000), "a b c");
    }

    #[test]
    fn chunks_overlap_and_skip_empty() {
        // window 60s, overlap 10s ⇒ step 50s. Cues at 0s and 55s.
        let tr = t(vec![cue(0, "one"), cue(55_000, "two")], vec![]);
        let chunks = tr.chunks(60, 10);
        // win [0,60s) has both "one" and "two"; win [50,110s) has "two".
        assert_eq!(chunks[0], Chunk { start_ms: 0, end_ms: 60_000, text: "one two".into() });
        assert_eq!(chunks[1], Chunk { start_ms: 50_000, end_ms: 110_000, text: "two".into() });
    }

    #[test]
    fn chunks_for_analysis_uses_chapters() {
        let tr = t(
            vec![cue(0, "intro"), cue(30_000, "middle"), cue(90_000, "end")],
            vec![Chapter { start_ms: 0, title: "A".into() }, Chapter { start_ms: 60_000, title: "B".into() }],
        );
        let chunks = tr.chunks_for_analysis();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].text, "intro middle");
        assert_eq!(chunks[1].text, "end");
    }
}

// ── Acquisition orchestrators ──

use crate::cmd::media::error::MediaError;
use crate::cmd::media::resolve;
use std::process::Command;

/// Subtitle language preference chain (highest priority first).
const SUB_LANG_PREF: &str = "zh-Hant,zh-Hans,zh,en";

fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// Get a transcript for `source`, using the on-disk cache when present.
pub fn get(mur_home: &Path, source: &str) -> Result<Transcript, MediaError> {
    let cache = cache_path(mur_home, source);
    if let Ok(body) = std::fs::read_to_string(&cache)
        && let Ok(tr) = serde_json::from_str::<Transcript>(&body)
    {
        return Ok(tr);
    }
    let tr = if is_url(source) {
        fetch_youtube(mur_home, source)?
    } else {
        read_local(source)?
    };
    if let Some(parent) = cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&tr) {
        let tmp = cache.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &cache);
        }
    }
    Ok(tr)
}

/// Fetch captions for a URL via yt-dlp json3 (+ chapters from info-json).
fn fetch_youtube(mur_home: &Path, source: &str) -> Result<Transcript, MediaError> {
    let ytdlp = resolve::detect_ytdlp().ok_or(MediaError::YtdlpMissing)?;
    let work = mur_home.join("runtime").join("yt-work");
    let _ = std::fs::create_dir_all(&work);
    let out_tpl = work.join("sub.%(ext)s");
    let status = Command::new(&ytdlp)
        .args([
            "--skip-download",
            "--write-subs",
            "--write-auto-subs",
            "--sub-lang",
            SUB_LANG_PREF,
            "--sub-format",
            "json3",
            "--write-info-json",
            "-o",
        ])
        .arg(&out_tpl)
        .arg(source)
        .status()
        .map_err(|_| MediaError::SourceUnresolvable)?;
    if !status.success() {
        return Err(MediaError::SourceUnresolvable);
    }
    let mut cues = Vec::new();
    let mut lang = "und".to_string();
    let mut chapters = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&work) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json3") {
                if let Ok(body) = std::fs::read_to_string(e.path()) {
                    cues = parse_json3(&body);
                    lang = name
                        .trim_start_matches("sub.")
                        .trim_end_matches(".json3")
                        .to_string();
                }
            } else if name.ends_with(".info.json") {
                if let Ok(body) = std::fs::read_to_string(e.path()) {
                    chapters = parse_info_chapters(&body);
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    if cues.is_empty() {
        return Err(MediaError::NoTranscript);
    }
    Ok(Transcript { source_id: source.to_string(), lang, cues, chapters })
}

/// Extract chapters from a yt-dlp info-json (`chapters: [{start_time, title}]`).
fn parse_info_chapters(json: &str) -> Vec<Chapter> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("chapters")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|ch| {
                    let start = ch.get("start_time").and_then(|s| s.as_f64())?;
                    let title = ch.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    Some(Chapter { start_ms: (start * 1000.0) as i64, title })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read a local file's transcript: sidecar .srt/.vtt, else embedded track via ffmpeg.
fn read_local(path: &str) -> Result<Transcript, MediaError> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(MediaError::SourceUnresolvable);
    }
    for ext in ["srt", "vtt"] {
        let sidecar = p.with_extension(ext);
        if let Ok(body) = std::fs::read_to_string(&sidecar) {
            let cues = if ext == "srt" { parse_srt(&body) } else { parse_vtt(&body) };
            if !cues.is_empty() {
                return Ok(Transcript {
                    source_id: path.to_string(),
                    lang: "und".into(),
                    cues,
                    chapters: Vec::new(),
                });
            }
        }
    }
    if let Some(ffmpeg) = resolve::detect_ffmpeg() {
        let tmp = std::env::temp_dir().join("mur-embedded-sub.srt");
        let _ = std::fs::remove_file(&tmp);
        let ok = Command::new(ffmpeg)
            .args(["-y", "-i", path, "-map", "0:s:0"])
            .arg(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok && let Ok(body) = std::fs::read_to_string(&tmp) {
            let cues = parse_srt(&body);
            let _ = std::fs::remove_file(&tmp);
            if !cues.is_empty() {
                return Ok(Transcript {
                    source_id: path.to_string(),
                    lang: "und".into(),
                    cues,
                    chapters: Vec::new(),
                });
            }
        }
    }
    Err(MediaError::NoTranscript)
}
