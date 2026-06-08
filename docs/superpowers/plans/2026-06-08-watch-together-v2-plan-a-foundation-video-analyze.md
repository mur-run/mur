# Watch-Together v2 — Plan A: Foundation + `video-analyze` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a host-agnostic `video_analyze` capability — given a YouTube link or local
video, produce a structured zh-TW (locale-aware) summary / conclusions, plus the shared
media foundation (typed errors, transcript service, source resolution) it rests on.

**Architecture:** Pure, unit-tested building blocks (subtitle parsers, time-window
chunking, deep-link + markdown rendering, error→message mapping, cache key) under thin
I/O orchestrators (yt-dlp transcript fetch, local-LLM map-reduce). Follows the v1 media
module pattern exactly: pure functions get TDD; process/network/LLM I/O is build-only +
manual E2E. Exposed as one new MCP tool (`video_analyze`) and one skill manifest.

**Tech Stack:** Rust 2024, `mur-core` (`serde_json`, `sha2`, `regex`, `reqwest`,
`anyhow` — all already present, **no new deps**), `mur-mcp-server`. Local LLM via the
existing `mur_common::local_llm` endpoint. `yt-dlp` is an optional external binary.

**Spec:** `docs/superpowers/specs/2026-06-08-watch-together-v2-design.md` (§4, §5, §9 Plan A).

**Scope note:** This is Plan A of two. Plan B (proactive co-watching, runtime-only) is a
separate plan and depends on this foundation. Plan A delivers value on its own.

---

## File Structure

**Create:**
- `mur-core/src/cmd/media/error.rs` — `MediaError` enum + warm user-facing messages.
- `mur-core/src/cmd/media/transcript.rs` — `Cue`/`Chapter`/`Transcript`/`Chunk` types,
  json3 / srt / vtt parsers, `window` / `chunks`, cache, acquisition orchestrators.
- `mur-core/src/cmd/media/resolve.rs` — yt-dlp / VLC detection, `last_source`
  persistence, DRM host heuristic.
- `mur-core/src/cmd/media/analyze.rs` — analysis modes, deep-link + markdown render,
  result parsing, map-reduce orchestrator.
- `mur-core/src/skills/video_analyze.yaml` — skill manifest.

**Modify:**
- `mur-core/src/cmd/media/mod.rs` — declare new submodules; add `local_base_url` helper.
- `mur-core/src/cmd/media/vlc.rs` — `open()` persists `last_source`.
- `mur-mcp-server/src/tools.rs` — register `video_analyze` tool def + dispatch + test.
- `mur-core/src/cmd/sync_cmd.rs` — register the new skill in `ensure_mur_skill`.

**Deviation from spec §4.2:** the spec mentions storing `last_source` inside
`watch.json`. To keep Plan A independent of Plan B's `WatchSession`, Plan A stores it in
a standalone `~/.mur/runtime/last_source` text file. Plan B will read/supersede it.

---

## Phase 0 — Errors + module scaffolding

### Task 1: `MediaError` with warm user messages

**Files:**
- Create: `mur-core/src/cmd/media/error.rs`
- Modify: `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/media/error.rs`:

```rust
//! Typed media errors with warm, actionable, locale-aware user messages.

use std::fmt;

/// All recoverable media failures. `user_message()` is what the agent relays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    VlcNotFound,
    VlcHttpDown,
    DrmProtected,
    NoTranscript,
    ModelOffline,
    SnapshotFailed,
    SourceUnresolvable,
    YtdlpMissing,
}

impl MediaError {
    /// Warm zh-TW message + actionable hint. (zh-TW is the product default brand voice.)
    pub fn user_message(&self) -> &'static str {
        match self {
            MediaError::VlcNotFound => "我找不到 VLC，請先安裝 VLC.app 再試一次。",
            MediaError::VlcHttpDown => "VLC 沒有回應，請確認它正在執行。",
            MediaError::DrmProtected => "這是有 DRM 保護的串流，我沒辦法擷取畫面或字幕喔。",
            MediaError::NoTranscript => "這支影片找不到字幕，所以我沒辦法做文字分析。",
            MediaError::ModelOffline => "本地模型還沒就緒（MuR Hub 有啟動嗎？）。",
            MediaError::SnapshotFailed => "我擷取畫面失敗了，稍後再試一次。",
            MediaError::SourceUnresolvable => "我解析不了這個來源，請確認連結或檔案路徑。",
            MediaError::YtdlpMissing => "要分析 YouTube 影片需要 yt-dlp，安裝後再試一次。",
        }
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.user_message())
    }
}

impl std::error::Error for MediaError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_nonempty_message() {
        let all = [
            MediaError::VlcNotFound,
            MediaError::VlcHttpDown,
            MediaError::DrmProtected,
            MediaError::NoTranscript,
            MediaError::ModelOffline,
            MediaError::SnapshotFailed,
            MediaError::SourceUnresolvable,
            MediaError::YtdlpMissing,
        ];
        for e in all {
            assert!(!e.user_message().is_empty(), "empty message for {e:?}");
            // Display delegates to user_message.
            assert_eq!(format!("{e}"), e.user_message());
        }
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/media/mod.rs`, add `pub mod error;` next to the existing
`pub mod scene;` / `pub mod vlc;` lines.

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p mur-core media::error::tests`
Expected: PASS (1 test).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/error.rs mur-core/src/cmd/media/mod.rs
git commit -m "feat(media): typed MediaError with warm user messages"
```

---

## Phase 1 — Transcript service (pure parsers TDD; I/O build-only)

### Task 2: Transcript types + json3 parser

**Files:**
- Create: `mur-core/src/cmd/media/transcript.rs`
- Modify: `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/media/transcript.rs`:

```rust
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
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/media/mod.rs`, add `pub mod transcript;`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p mur-core media::transcript::json3_tests`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/transcript.rs mur-core/src/cmd/media/mod.rs
git commit -m "feat(media): transcript types + yt-dlp json3 parser"
```

---

### Task 3: SRT + VTT parsers

**Files:**
- Modify: `mur-core/src/cmd/media/transcript.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/cmd/media/transcript.rs`:

```rust
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
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core media::transcript::subtitle_tests`
Expected: PASS (3 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/media/transcript.rs
git commit -m "feat(media): SRT/VTT subtitle parsers"
```

---

### Task 4: `window` + `chunks` (+ chapter alignment)

**Files:**
- Modify: `mur-core/src/cmd/media/transcript.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/cmd/media/transcript.rs`:

```rust
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
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core media::transcript::chunk_tests`
Expected: PASS (3 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/media/transcript.rs
git commit -m "feat(media): transcript window + time-window/chapter chunking"
```

---

### Task 5: Cache key (TDD) + acquisition orchestrators (build-only)

**Files:**
- Modify: `mur-core/src/cmd/media/transcript.rs`

- [ ] **Step 1: Write the failing test for the cache key**

Append to `mur-core/src/cmd/media/transcript.rs`:

```rust
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
```

- [ ] **Step 2: Run the cache test**

Run: `cargo test -p mur-core media::transcript::cache_tests`
Expected: PASS (1 test).

- [ ] **Step 3: Add acquisition orchestrators (build-only — process/network I/O)**

Append to `mur-core/src/cmd/media/transcript.rs`:

```rust
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
    // Find the produced .json3 (named sub.<lang>.json3) and info.json.
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
    // Embedded subtitle track via ffmpeg (best-effort).
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
```

- [ ] **Step 4: Verify the crate builds** (Task 6 adds the `resolve` helpers this calls; until then `resolve::detect_*` is unresolved, so this step only fully passes after Task 6)

Run: `cargo build -p mur-core` after Task 6.
Expected: builds.

> **Sequencing note:** Steps 3 references `resolve::detect_ytdlp` / `detect_ffmpeg`,
> created in Task 6. Commit this task's code now; the build is verified at the end of
> Task 6. The cache test (Steps 1–2) passes independently.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/media/transcript.rs
git commit -m "feat(media): transcript cache + yt-dlp/local acquisition orchestrators"
```

---

## Phase 2 — Source resolution + last_source

### Task 6: `resolve.rs` — tool detection, DRM heuristic, last_source

**Files:**
- Create: `mur-core/src/cmd/media/resolve.rs`
- Modify: `mur-core/src/cmd/media/mod.rs`, `mur-core/src/cmd/media/vlc.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/media/resolve.rs`:

```rust
//! Source resolution helpers: external-tool detection, DRM heuristic, last-source.

use std::path::{Path, PathBuf};

/// Detect yt-dlp: `MUR_YTDLP_PATH` override, else `yt-dlp` on PATH.
pub fn detect_ytdlp() -> Option<PathBuf> {
    detect_tool("MUR_YTDLP_PATH", "yt-dlp")
}

/// Detect ffmpeg: `MUR_FFMPEG_PATH` override, else `ffmpeg` on PATH.
pub fn detect_ffmpeg() -> Option<PathBuf> {
    detect_tool("MUR_FFMPEG_PATH", "ffmpeg")
}

fn detect_tool(env_key: &str, bin: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_key) {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    which_on_path(bin)
}

/// Minimal PATH search (avoids a new `which` crate dependency).
fn which_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Heuristic: does this URL host a DRM-protected streaming service we cannot capture?
pub fn is_drm_host(source: &str) -> bool {
    const DRM_HOSTS: &[&str] = &[
        "netflix.com",
        "disneyplus.com",
        "primevideo.com",
        "hbomax.com",
        "max.com",
        "hulu.com",
        "appletv.com",
    ];
    let lower = source.to_ascii_lowercase();
    DRM_HOSTS.iter().any(|h| lower.contains(h))
}

fn last_source_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("last_source")
}

/// Persist the most recently opened source string (plain text, atomic).
pub fn save_last_source(mur_home: &Path, source: &str) -> std::io::Result<()> {
    let path = last_source_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, source.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

/// Load the most recently opened source string, if any.
pub fn load_last_source(mur_home: &Path) -> Option<String> {
    let s = std::fs::read_to_string(last_source_path(mur_home)).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ytdlp_env_override_missing_is_none() {
        unsafe { std::env::set_var("MUR_YTDLP_PATH", "/no/such/yt-dlp") };
        assert_eq!(detect_ytdlp(), None);
        unsafe { std::env::remove_var("MUR_YTDLP_PATH") };
    }

    #[test]
    fn drm_hosts_detected() {
        assert!(is_drm_host("https://www.netflix.com/watch/123"));
        assert!(is_drm_host("https://DisneyPlus.com/x"));
        assert!(!is_drm_host("https://youtu.be/abc"));
        assert!(!is_drm_host("/home/me/movie.mkv"));
    }

    #[test]
    fn last_source_roundtrips() {
        let home = TempDir::new().unwrap();
        assert_eq!(load_last_source(home.path()), None);
        save_last_source(home.path(), "https://youtu.be/abc").unwrap();
        assert_eq!(load_last_source(home.path()).as_deref(), Some("https://youtu.be/abc"));
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/media/mod.rs`, add `pub mod resolve;`.

- [ ] **Step 3: Persist last_source on open**

In `mur-core/src/cmd/media/vlc.rs`, the `open` function currently ends with the
`send_command(&rt, client, "in_play", &[("input", source)]).await` call. Replace the
body of `open` so it records the source first:

```rust
/// Open a local file path or a URL (e.g. YouTube) in VLC.
#[allow(dead_code)]
pub async fn open(source: &str) -> Result<VlcStatus> {
    let client = super::shared_client();
    let home = mur_home()?;
    // Remember the original source so `video_analyze` (no arg) can resolve it later;
    // VLC's status.xml does not expose a usable source URI. (Spec §4.2.)
    let _ = super::resolve::save_last_source(&home, source);
    let rt = ensure_vlc_running(&home, client).await?;
    send_command(&rt, client, "in_play", &[("input", source)]).await
}
```

- [ ] **Step 4: Run the tests + build the crate (also verifies Task 5)**

Run: `cargo test -p mur-core media::resolve::tests`
Expected: PASS (3 tests).

Run: `cargo build -p mur-core`
Expected: builds (this resolves the `resolve::detect_*` references from Task 5).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/media/resolve.rs mur-core/src/cmd/media/mod.rs mur-core/src/cmd/media/vlc.rs
git commit -m "feat(media): tool detection, DRM heuristic, last_source persistence"
```

---

## Phase 3 — Analysis pipeline

### Task 7: Analysis types + mode parsing + deep links

**Files:**
- Create: `mur-core/src/cmd/media/analyze.rs`
- Modify: `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/cmd/media/analyze.rs`:

```rust
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
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/media/mod.rs`, add `pub mod analyze;`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p mur-core media::analyze::basics_tests`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/analyze.rs mur-core/src/cmd/media/mod.rs
git commit -m "feat(media): analysis modes + timestamp deep links"
```

---

### Task 8: Result parsing + markdown rendering

**Files:**
- Modify: `mur-core/src/cmd/media/analyze.rs`

- [ ] **Step 1: Write the failing test**

Append to `mur-core/src/cmd/media/analyze.rs`:

```rust
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
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p mur-core media::analyze::render_tests`
Expected: PASS (3 tests).

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/media/analyze.rs
git commit -m "feat(media): analysis result parsing + markdown rendering"
```

---

### Task 9: Map-reduce orchestrator (build-only)

**Files:**
- Modify: `mur-core/src/cmd/media/analyze.rs`, `mur-core/src/cmd/media/mod.rs`

- [ ] **Step 1: Add a shared `local_base_url` helper to mod.rs**

In `mur-core/src/cmd/media/mod.rs`, add (near the other helpers):

```rust
/// Resolve the local OpenAI-compatible model base URL (e.g. http://127.0.0.1:PORT/v1).
pub(crate) fn local_base_url() -> anyhow::Result<String> {
    let home = crate::cmd::resolve_mur_home()?;
    mur_common::local_llm::read_base_url(&home)
        .map_err(|_| anyhow::anyhow!("local model endpoint not available (is MuR Hub running?)"))
}
```

- [ ] **Step 2: Add the orchestrator (build-only — LLM I/O verified in manual E2E)**

Append to `mur-core/src/cmd/media/analyze.rs`:

```rust
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
            "你是影片分析助手。根據以下各段重點，輸出嚴格 JSON：{\"topic\":..,\"key_points\":[{\"text\":..,\"t_ms\":..}],\"key_moments\":[{\"text\":..,\"t_ms\":..}],\"conclusion\":\"深入的分析與結論\"}。只輸出 JSON，用繁體中文。"
        }
        _ => {
            "你是影片分析助手。根據以下各段重點，輸出嚴格 JSON：{\"topic\":..,\"key_points\":[{\"text\":..,\"t_ms\":..}],\"key_moments\":[{\"text\":..,\"t_ms\":..}],\"conclusion\":\"摘要\"}。只輸出 JSON，用繁體中文。"
        }
    }
}

/// Build an OpenAI-compatible chat request (text-only, temperature 0 for determinism).
fn chat_request(system: &str, user: &str) -> serde_json::Value {
    json!({
        "model": DEFAULT_BUNDLED_MODEL_ID,
        "temperature": 0,
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
```

- [ ] **Step 3: Verify the crate builds**

Run: `cargo build -p mur-core`
Expected: builds. (If `parse_completion` is not `pub`, make it `pub` in
`mur-core/src/cmd/media/scene.rs` — it already is per the v1 code.)

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/media/analyze.rs mur-core/src/cmd/media/mod.rs
git commit -m "feat(media): video-analyze map-reduce orchestrator (local LLM)"
```

---

## Phase 4 — MCP tool + skill wiring

### Task 10: Register the `video_analyze` MCP tool

**Files:**
- Modify: `mur-mcp-server/src/tools.rs`

- [ ] **Step 1: Add the tool definition**

In `mur-mcp-server/src/tools.rs`, immediately after the `scene_explain` `Tool { … }`
block (ends near line 222, before `// ── compress tools ──`), add:

```rust
        Tool {
            name: "video_analyze".into(),
            description: "Analyze a whole video (YouTube link or local file) and return a structured zh-TW summary or conclusions with clickable timestamps. Uses captions + the local model. Omit 'source' to analyze the currently open video.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(BTreeMap::from([
                    ("source".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Video URL or local path; omit to use the currently open video".into(),
                        default: None,
                    }),
                    ("mode".into(), ToolParam {
                        param_type: "string".into(),
                        description: "summary (default) | conclusions | qa".into(),
                        default: None,
                    }),
                    ("focus".into(), ToolParam {
                        param_type: "string".into(),
                        description: "For qa mode: the question to answer".into(),
                        default: None,
                    }),
                ])),
                required: None,
            },
        },
```

- [ ] **Step 2: Add the dispatch arm**

In the same file, in `call_tool`, after the `"scene_explain" => { … }` arm (ends near
line 452), add:

```rust
        "video_analyze" => {
            let source = arguments.get("source").and_then(|v| v.as_str());
            let mode = arguments.get("mode").and_then(|v| v.as_str());
            let focus = arguments.get("focus").and_then(|v| v.as_str());
            let markdown = mur_core::cmd::media::analyze::analyze(source, mode, focus)
                .await
                .map_err(|e| format!("video_analyze failed: {}", e))?;
            Ok(json!({ "analysis": markdown }))
        }
```

- [ ] **Step 3: Extend the registration test**

In `mur-mcp-server/src/tools.rs`, update `media_tools_registered` to include the new
tool:

```rust
        for n in ["vlc_open", "vlc_playback", "vlc_status", "scene_explain", "video_analyze"] {
```

- [ ] **Step 4: Run the test + build the server**

Run: `cargo test -p mur-mcp-server media_tool_tests`
Expected: PASS.

Run: `cargo build -p mur-mcp-server`
Expected: builds.

- [ ] **Step 5: Commit**

```bash
git add mur-mcp-server/src/tools.rs
git commit -m "feat(mcp): register video_analyze tool"
```

---

### Task 11: `video-analyze` skill manifest

**Files:**
- Create: `mur-core/src/skills/video_analyze.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs`

- [ ] **Step 1: Create the manifest**

Create `mur-core/src/skills/video_analyze.yaml`:

```yaml
name: video-analyze
version: 0.1.0
publisher: human:mur
description: "Analyze a whole video — produce a structured summary, conclusions, or answer a question — from its captions, using the local model."
category: media
hosts: [all]
content:
  abstract: |
    When the user wants the gist, takeaways, or conclusions of a video (YouTube link or
    local file), call video_analyze(source?, mode?, focus?). Omit source to analyze the
    currently open video. mode = summary | conclusions | qa (qa needs focus).
  context: |
    # video-analyze — summarize / draw conclusions from a video

    Use video_analyze when the user asks to "summarize this video", "what are the key
    points / conclusions", or "what does it say about X".
    - mode=summary (default): overview + key points.
    - mode=conclusions: deeper analysis / takeaways.
    - mode=qa with focus="<question>": answer from the transcript.
    Cite the timestamps it returns. Analysis runs on the local model; fetching YouTube
    captions contacts YouTube but uploads none of the user's own content. DRM streaming
    services (Netflix etc.) cannot be analyzed — decline gracefully.
tags: [mur, media, video, analysis, builtin]
triggers:
  - type: keyword
    pattern: "(總結|摘要|重點|結論|分析).{0,8}(影片|這部|這支|video)|summari[sz]e.{0,12}video|key (points|takeaways)|這(部|支)影片在(講|說)什麼"
  - type: manual
priority: normal
```

- [ ] **Step 2: Register the skill**

In `mur-core/src/cmd/sync_cmd.rs`, in `ensure_mur_skill`'s `skills` array, after the
`("scene-explain", …)` entry, add:

```rust
        (
            "video-analyze",
            include_str!("../skills/video_analyze.yaml"),
        ),
```

- [ ] **Step 3: Verify the manifest parses and the crate builds**

Run: `python3 -c "import yaml; yaml.safe_load(open('mur-core/src/skills/video_analyze.yaml')); print('ok')"`
Expected: `ok`.

Run: `cargo build -p mur-core`
Expected: builds (fails loudly if the `include_str!` path is wrong).

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/skills/video_analyze.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(media): video-analyze skill manifest + registration"
```

---

### Task 12: Full verification + manual E2E

**Files:** none (verification only)

- [ ] **Step 1: Workspace build**

Run: `cargo build --workspace`
Expected: builds (the `mur-agent-gui` crate is workspace-excluded by design).

- [ ] **Step 2: Targeted media tests (avoids known-flaky unrelated tests)**

Run: `cargo test -p mur-core media::`
Expected: PASS — all `media::error`, `media::transcript`, `media::resolve`,
`media::analyze` tests green.

- [ ] **Step 3: Lint + format**

Run: `cargo clippy -p mur-core -p mur-mcp-server -- -D warnings`
Expected: no warnings.

Run: `cargo fmt --check`
Expected: clean (run `cargo fmt` and amend if not).

- [ ] **Step 4: Manual E2E (requires MuR Hub running + yt-dlp installed)**

1. Ensure the local model endpoint is up (MuR Hub running) and `yt-dlp` is on PATH.
2. From an MCP host, call `video_analyze` with a short YouTube link that has captions,
   `mode="conclusions"`.
3. Expected: a zh-TW markdown analysis with `?t=Ns` deep links; second run is faster
   (transcript cache hit at `~/.mur/runtime/transcripts/`).
4. Call `vlc_open` on a YouTube link, then `video_analyze` with no `source`.
   Expected: it analyzes that video (resolved via `~/.mur/runtime/last_source`).
5. Call `video_analyze` on a Netflix URL.
   Expected: graceful DRM decline message.
6. Temporarily rename `yt-dlp` off PATH, call `video_analyze` on a URL.
   Expected: the friendly `YtdlpMissing` message; VLC playback (`vlc_open`) still works.

- [ ] **Step 5: Final commit (if fmt/clippy required changes)**

```bash
git add -A
git commit -m "chore(media): clippy/fmt fixups for video-analyze"
```

---

## Spec Coverage Check (Plan A scope)

- Spec §4.1 TranscriptService (json3/srt/vtt, window, chunks, cache) → Tasks 2–5.
- Spec §4.2 SourceResolver (yt-dlp/ffmpeg detect, DRM, last_source) → Task 6.
- Spec §4.3 MediaError → Task 1 (used throughout).
- Spec §5 video-analyze (pipeline, modes, deep links, structured output, MCP tool,
  skill) → Tasks 7–11. `qa` is wired (mode parsed, param plumbed) but kept shallow per
  spec; deepening `qa` retrieval is a follow-up.
- Spec §9 Plan A file list → all files covered; **no new crate dependencies** (Whisper
  deferred per spec §7; `image` belongs to Plan B).
- Out of Plan A scope (Plan B): `WatchSession`, `WatchScheduler`, `watch_*` tools,
  `watch-together` skill, snapshot lifecycle, `image` dep.
