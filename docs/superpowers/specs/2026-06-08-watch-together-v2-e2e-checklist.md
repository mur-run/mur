# Watch-Together v2 — Manual E2E Checklist

**Date:** 2026-06-08
**Scope:** Manual end-to-end verification of the shipped Watch-Together v2 (Plan A
`video_analyze` + Plan B proactive co-watching). Automated tests + build/clippy/fmt are
green in CI; this checklist covers the live paths that are build-only in CI (yt-dlp,
local model, VLC, the runtime scheduler).

**How to invoke the tools:** they are MCP tools served by `mur-mcp-server`. Drive them
from an MCP host connected to that server (e.g. Claude Code, or a murmur agent). Plan B
(proactive) additionally requires MuR to be running **as a murmur agent runtime** with a
user-facing delivery channel (companion voice/notification) — see §3.

Predecessor design: `2026-06-08-watch-together-v2-design.md`.

## 0. Prerequisites

- [ ] `brew install yt-dlp` (required to analyze YouTube in Plan A). `ffmpeg` optional
      (local embedded-subtitle extraction).
- [ ] VLC installed (`/Applications/VLC.app`) or `MUR_VLC_PATH` set.
- [ ] Rebuild + install from `main`: `./install.sh` (ships the new `mur`, MCP server,
      and skills).
- [ ] MuR Hub running and the local model endpoint up (needed by `video_analyze` and
      `scene_explain`).
- [ ] Skills synced: `mur agent skill list` shows `video-analyze` and `watch-together`.
- [ ] MCP host lists 18 tools, including `video_analyze` and
      `watch_start` / `watch_stop` / `watch_mute` / `watch_status`.

## 1. Plan A — `video_analyze` (single machine)

- [ ] **A1 Summary:** `video_analyze(source="<YouTube link with captions>")` → returns
      a structured zh-TW summary; key points carry `?t=Ns` deep links.
- [ ] **A2 Conclusions:** `video_analyze(source=..., mode="conclusions")` → deeper
      analysis / takeaways.
- [ ] **A3 Cache:** rerun the same link → noticeably faster; a file exists at
      `~/.mur/runtime/transcripts/<sha256>.json`.
- [ ] **A4 Current video:** `vlc_open("<YouTube>")`, then `video_analyze` with **no**
      `source` → analyzes that video (resolved via `~/.mur/runtime/last_source`).
- [ ] **A5 DRM decline:** `video_analyze(source="https://www.netflix.com/watch/123")`
      → "這是有 DRM 保護的串流…".
- [ ] **A6 yt-dlp missing:** move `yt-dlp` off PATH, analyze a YouTube link →
      "要分析 YouTube 影片需要 yt-dlp…"; and `vlc_open` playback still works.
- [ ] **A7 Local file:** `video_analyze` on a `movie.mp4` with a sibling `movie.srt`
      → analyzed from the sidecar (also exercises CRLF subtitle parsing — a CRLF file
      must yield all cues, not just the first).
- [ ] **A8 No captions:** a video with no subtitles → "這支影片找不到字幕…"
      (`NoTranscript`).

## 2. Plan B — proactive co-watching (requires a runtime agent + delivery channel)

- [ ] **B0:** start MuR as a murmur agent runtime (e.g. the concierge) with the
      companion voice/notification channel available.
- [ ] **B1 Start:** `vlc_open("<non-DRM video>")` and play → `watch_start`.
- [ ] **B2 Consent:** on the first large scene change, a consent question is delivered
      via voice/notification ("我可以在劇情轉折時…說「噓」可隨時靜音").
- [ ] **B3 Interjections:** subsequent big scene changes produce occasional one-line
      comments, **no more often than ~45s apart** (cooldown).
- [ ] **B4 Mute:** say "噓" or call `watch_mute` → comments stop; `watch_status` shows
      `muted: true`.
- [ ] **B5 No accumulation:** `~/.mur/runtime/vlc-snapshots/` does not grow unbounded
      (each frame deleted after hashing).
- [ ] **B6 Stop:** `watch_stop` → no further activity; `watch.json` shows
      `active: false`.
- [ ] **B7 Mute race (optional):** call `watch_mute` during the few-second capture
      window of an interjection → the mute is NOT clobbered (lost-mute fix).
- [ ] **B8 Quiet hours (optional):** set the agent's `quiet_hours` to cover "now"
      (including an overnight window like 22:00–08:00) → no interjections.

## 3. Diagnosis quick-reference

| Symptom / message | Likely cause |
|---|---|
| "我找不到 VLC…" | VLC not installed / `MUR_VLC_PATH` wrong |
| "本地模型還沒就緒…" | Hub / local model endpoint not running |
| "要分析 YouTube 影片需要 yt-dlp…" | yt-dlp not on PATH / `MUR_YTDLP_PATH` |
| "這支影片找不到字幕…" | No captions (Whisper fallback is deferred, spec §7) |
| Interjections never appear | No delivery channel (B0 unmet), or not running as a runtime agent |

## 4. Known deferrals (not expected to work)

- Idle auto-pause/resume (no real user-presence signal yet).
- Transcript-window grounding of interjections (runtime has no transcript access).
- `qa` mode is shallow (ignores `focus`; behaves like `summary`).
- Whisper audio-transcription fallback.
