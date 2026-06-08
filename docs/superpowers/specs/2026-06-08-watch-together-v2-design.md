# MuR Watch-Together v2 — Proactive Co-Watching + Video Analysis

**Status:** Design
**Date:** 2026-06-08
**Platform:** macOS first; cross-platform where VLC / yt-dlp are available.
**Predecessor:** `2026-06-02-companion-media-skills-design.md` (v1, shipped).
**Companion spec:** `2026-06-02-self-contained-hub-install-design.md` (owns onboarding/UX framing).

## 1. Problem / Goal

v1 watch-together is shipped and deliberately **passive**: the agent opens a source,
controls playback, and explains the current frame **only when asked** (`vlc-control` +
`scene-explain` skills, four MCP tools, `mur-core/src/cmd/media/`). v2 makes MuR a
**proactive, smarter watch buddy** while keeping everything local-first:

- **Pillar A — `video-analyze` (new skill):** produce a structured **summary /
  conclusions / analysis / Q&A** for a whole video, not just a single frame.
- **Pillar B — proactive co-watching:** **visual scene-change detection** drives
  *gentle, rate-limited interjections*; **idle auto-pause/resume** when the user steps
  away. Proactivity is consent-gated and silenceable ("噓").
- **Robustness:** typed errors + graceful degradation as a first-class best practice.

Design constraint (unchanged from v1): explanation and summarization run on the
**local bundled multimodal model** (default `Qwen3.5-2B`). No frame is uploaded. The
one honest network nuance: fetching YouTube captions/audio contacts YouTube — it does
**not** upload the user's own content (§6).

## 2. Research Basis (2026-06)

Web research on 2026 best practices for YouTube/video analysis informed Pillar A:

- **Transcript extraction — captions-first, audio→Whisper fallback, cache.** The
  standard pattern fetches existing captions first and only transcribes audio with
  Whisper when none exist; transcripts are cached keyed by a hash of source+config so
  re-runs skip extraction.
- **Chunking.** Default to sentence-boundary chunks (~200–400 tokens, 20–40 token
  overlap). When timestamps matter, use **fixed 60-second time-windows** with overlap
  and keep timestamps in metadata — **avoid raw caption segment boundaries** (uneven,
  context-poor). Overlap is the consistent theme that prevents context loss at seams.
- **Long video — map-reduce.** Map each chunk to key points, then reduce chunk
  summaries into a structured output (topic / key points / details / conclusions). RAG
  over embedded chunks is the upgrade for very long or queryable content (out of scope
  for v2, §7).
- **Determinism.** Use temperature 0 for summarization consistency.
- **Timestamp traceability.** Preserve timestamps so takeaways link back to the exact
  moment via the `&t=` URL parameter.
- **Structured prompts.** Specify the exact output shape (topic, key points, key
  moments, conclusion) and offer style/mode variants.
- **Chapter-aware structuring.** When the source carries chapter markers, align to them
  and verify generated boundaries adhere to source timestamps (ARC-Chapter pattern).

Sources: Apify "YouTube Transcripts for LLM and RAG Pipelines (2026)"; `martinopiaggi/
summarize`; `siddharthsky/AI-Video-Summarizer` (timestamps); Ollama local-LLM
summarizer (Medium, ausaafahmad); `arxiv.org/abs/2511.14349` (ARC-Chapter);
`arxiv.org/abs/2405.13382` (VTG-LLM).

## 3. Architecture

v2 keeps the v1 model — **skill manifests teach *when/why*; MCP tools *act*** — and adds
a shared foundation under two feature pillars.

```
                         ┌──────────────────────────────────────────┐
                         │  Shared foundation (new)                  │
                         │  TranscriptService · SourceResolver ·     │
                         │  MediaError (typed, graceful degradation) │
                         └───────────────┬──────────────┬───────────┘
                                         │              │
        Pillar A (offline, pull)         │              │   Pillar B (real-time, push)
   ┌──────────────────────────────┐      │              │   ┌──────────────────────────────┐
   │ video_analyze(source,mode)   │◄─────┘              └──►│ WatchSession + WatchTrigger   │
   │ transcript→chunk→map→reduce  │                         │ snapshot→dHash diff→interject │
   │ → structured summary/concl.  │                         │ + C6 idle auto-pause          │
   └──────────────────────────────┘                         └──────────────────────────────┘
```

- **Pull vs push.** v1 tools are pull (agent calls them). Pillar B is push: a
  `WatchTrigger` loop (modeled on the existing `mur-agent-runtime` `IdleScheduler`,
  C6) polls VLC and **injects** a narration prompt into the agent's `TaskRunner` when
  an interjection-worthy moment is detected.
- **Reuse.** Rate-limiting/consent reuse the companion `Outbox` cooldown model and
  `earned_permission` gates (enabled / paused / learning / quiet_hours). Idle
  auto-pause reuses C6 `IdleTrigger` + `should_fire`.

## 4. Shared Foundation (new)

### 4.1 `TranscriptService` (`mur-core/src/cmd/media/transcript.rs`)

Data model:

```
struct Cue { start_ms: i64, end_ms: i64, text: String }
struct Chapter { start_ms: i64, title: String }
struct Transcript { source_id: String, lang: String, cues: Vec<Cue>, chapters: Vec<Chapter> }
```

Acquisition (**captions-first**):

1. **YouTube / URL:** `yt-dlp --skip-download --write-subs --write-auto-subs
   --sub-lang <pref> --sub-format json3 --write-info-json -o <tmp> URL`. Prefer manual
   subs over auto-generated; parse json3 `events` → `Cue`s; read chapters from
   info-json. Requires yt-dlp (§5; absence degrades gracefully).
2. **Local file:** sidecar `.srt`/`.vtt` next to the media → embedded subtitle track
   (when `ffmpeg` is present: `-map 0:s:0`) → **optional** local Whisper fallback
   (only if a whisper binary/endpoint is configured; otherwise `NoTranscript`).

Caching: `~/.mur/runtime/transcripts/<sha256(source)>.json`. Cache hit skips
acquisition.

API:

- `get(source: &str) -> Result<Transcript, MediaError>`
- `Transcript::window(t_ms, span_ms) -> String` — concatenated cue text around a time
  (powers live "他剛說的是什麼意思？").
- `Transcript::chunks(window_secs, overlap_secs) -> Vec<Chunk>` — fixed time-window
  chunks with overlap, timestamps preserved (powers analysis). Chapter-aligned when
  chapters exist.

**No hardcoded values:** `sub-lang` preference derives from user locale (zh-TW → en
fallback); window width / overlap / poll intervals are named config constants.

### 4.2 `SourceResolver` + tool detection

- VLC detection: existing macOS path + `MUR_VLC_PATH`, plus best-effort Windows
  (`%ProgramFiles%\VideoLAN\VLC\vlc.exe`) and Linux (`which vlc`).
- yt-dlp detection: `MUR_YTDLP_PATH` else `which yt-dlp`. Optional dependency.
- DRM heuristic: known DRM hosts / VLC open failure → `DrmProtected` (decline).

### 4.3 `MediaError` (typed, graceful degradation)

```
enum MediaError {
  VlcNotFound, VlcHttpDown, DrmProtected, NoTranscript,
  ModelOffline, SnapshotFailed, SourceUnresolvable, YtdlpMissing,
}
```

Each variant maps to a **warm zh-TW user message + actionable hint** (e.g.
`YtdlpMissing` → "要分析 YouTube 影片需要 yt-dlp，安裝後再試一次"; `DrmProtected` →
graceful decline). The MCP layer returns these as tool errors; the agent relays the
friendly message.

## 5. Pillar A — `video-analyze` skill

Pipeline (research-backed, §2):

1. `TranscriptService.get(source)` (source omitted ⇒ current VLC source).
2. **Chunk:** chapter-aligned if chapters exist, else 60s time-windows with overlap;
   timestamps preserved.
3. **Map:** local model, **temperature 0**, per chunk → key points + notable
   timestamps.
4. **Reduce:** structured output → **topic / key-point list (with timestamps) / key
   moments / conclusion & analysis**.

Modes (`mode` param):

- `summary` (default) — overview + key points.
- `conclusions` — analysis / takeaways / "結論".
- `qa` — requires `focus` (a question); keyword-retrieve relevant cues, answer with
  cited timestamps.

Output: structured `serde` value **and** rendered markdown — YouTube uses `&t=` deep
links, local files use `[mm:ss]`. Language follows user locale.

Surface:

- **MCP tool:** `video_analyze(source?, mode?, focus?)`.
- **Skill manifest:** `mur-core/src/skills/video_analyze.yaml` — teaches when to use
  ("總結這支影片 / 重點 / 結論 / 他在講什麼"), to cite timestamps, and the privacy note
  (§6). Triggers: keyword (zh + en) + manual.

## 6. Pillar B — Proactive Co-Watching

### 6.1 `WatchSession` (runtime-persisted)

Shared between the trigger loop and MCP tools:

```
struct WatchSession {
  active: bool, source: String, muted: bool,
  last_interjection_ms: i64, last_scene_phash: u64,
  consent: Consent,            // Unasked | Granted | Declined
}
```

Persisted under `~/.mur/runtime/watch.json` (same pattern as `vlc.json`).

### 6.2 `WatchTrigger` (push; modeled on `IdleScheduler`)

Spawned when a session starts. Every `scene_poll_secs` (default ~6s) while VLC
`state == playing`:

1. Snapshot the current frame → compute a **perceptual hash (dHash)**.
2. Hamming distance vs `last_scene_phash`.
3. If distance ≥ `scene_change_threshold` **and** `(now − last_interjection_ms) ≥
   interjection_cooldown_secs` **and** `!muted` **and** `consent == Granted`:
   inject a narration prompt into the `TaskRunner` — frame data-url + `window(now,
   ±span)` transcript + "briefly narrate this turn." Update `last_interjection_ms` and
   `last_scene_phash`.
4. If `consent == Unasked` on the first large change: inject a **one-time consent ask**
   instead ("要我在劇情轉折時插話嗎？說「噓」可隨時靜音"). Record `Granted`/`Declined`.

Rate-limit/consent reuse the `Outbox` cooldown model + `earned_permission` gates
(including quiet hours). `av-scenechange` is heavier (needs a decode stream);
snapshot + dHash is sufficient and is what v2 uses.

### 6.3 Idle auto-pause/resume

Register a C6 `IdleTrigger` (`after_secs` default 180) whose action calls
`vlc playback pause`; on the user's return, the agent offers to resume (auto-resume is
a config option). Quiet hours are already honored by `should_fire`.

### 6.4 Mute / "噓" and session control

- **MCP tools:** `watch_start(source)`, `watch_stop`, `watch_mute`, `watch_status`.
- **Skill manifest:** `mur-core/src/skills/watch_together.yaml` — teaches session
  semantics and etiquette (ask consent before interjecting, "噓" = mute immediately,
  decline DRM). `vlc-control` and `scene-explain` manifests stay as-is (low-level
  control + on-demand explain).

## 7. Out of Scope (YAGNI v2)

- RAG / vector retrieval over full transcripts (map-reduce suffices for v2).
- `factcheck` mode.
- Continuous live-commentator narration (Pillar B is gentle/rate-limited only).
- A Hub GUI watch panel (CLI/MCP/companion layer only this round).
- Bundling yt-dlp or Whisper (licensing/size — document install instead).
- Multi-frame temporal "frame-grid" reasoning; non-VLC players.

## 8. Testing

- **Unit:** json3 parse; srt/vtt parse; `Cue::window`; `chunks` (window + overlap +
  chapter alignment); dHash + Hamming distance; trigger gating (`should_fire`-style
  cooldown/consent/mute/quiet-hours); `MediaError` → message mapping; manifest
  validity.
- **Integration:** json3 fixture → `Transcript`; mock LLM map-reduce → structured
  output with timestamps; synthetic phash change + cooldown → exactly one injection;
  idle trigger → `vlc pause`.
- **Manual E2E:** analyze a YouTube link → zh-TW conclusions with `&t=` links; start a
  session → scene change → consent ask → interjection; "噓" silences; DRM declined
  gracefully; yt-dlp missing → friendly decline but playback still works.

## 9. Affected Components & Plan Split

Two implementation plans share one spec:

- **Plan A — Foundation + analysis:** `TranscriptService`, `SourceResolver`,
  `MediaError`; `video_analyze` tool + `video-analyze` skill. Files:
  `mur-core/src/cmd/media/{transcript.rs,resolve.rs,error.rs}`,
  `mur-mcp-server/src/tools.rs`, `mur-core/src/skills/video_analyze.yaml`,
  `mur-core/src/cmd/sync_cmd.rs` (register skill).
- **Plan B — Proactive co-watching:** `WatchSession`, `WatchTrigger`, idle auto-pause,
  mute; `watch_*` tools + `watch-together` skill. Files:
  `mur-core/src/cmd/media/watch.rs`, `mur-agent-runtime` (trigger + idle wiring),
  `mur-mcp-server/src/tools.rs`, `mur-core/src/skills/watch_together.yaml`,
  `mur-core/src/cmd/sync_cmd.rs`.

Rationale for the split: Plan A is offline/batch and self-contained; Plan B is
real-time/push and depends only on the shared foundation. They can be implemented,
tested, and shipped independently.
