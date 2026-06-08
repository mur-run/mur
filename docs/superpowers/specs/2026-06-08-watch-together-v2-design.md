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
  *gentle, rate-limited interjections*, consent-gated and silenceable ("噓"). This is a
  **runtime-only** capability (it can only run when MuR is itself a murmur agent
  runtime — see §3); it is not reachable through an external MCP host.
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
        Pillar A (offline, pull)         │              │   Pillar B (push, RUNTIME-ONLY)
   ┌──────────────────────────────┐      │              │   ┌──────────────────────────────┐
   │ video_analyze(source,mode)   │◄─────┘              └──►│ WatchScheduler (in runtime)   │
   │ transcript→chunk→map→reduce  │                         │ snapshot→dHash diff→inject    │
   │ → structured summary/concl.  │                         │ → TaskRunner → voice delivery │
   └──────────────────────────────┘                         └──────────────────────────────┘
        any MCP host (Claude Code,                              only when MuR runs as a
        murmur runtime, …)                                      murmur agent runtime (Hub)
```

- **Pull (Pillar A) works for any MCP host.** v1/Pillar A tools are pull: the host
  (Claude Code, a murmur runtime, …) calls the tool and gets a response.
- **Push (Pillar B) is runtime-only — a hard constraint.** MCP is client-initiated
  request/response: **an MCP server cannot start a turn on the host.** Proactive
  interjection therefore cannot be driven by an MCP tool. It lives *inside*
  `mur-agent-runtime` as a scheduler — a sibling of `IdleScheduler`, which holds an
  `Arc<TaskRunner>` and injects a turn via `runner.run_sync(..)`, wired in
  `supervisor.rs`. Pillar B only manifests when **MuR itself runs as a murmur agent
  runtime with a user-facing delivery channel** (companion voice/notification). For an
  external MCP client there is no proactivity — at most it can poll status.
- **MCP `watch_*` tools are state flags only.** They flip fields in `watch.json`; the
  runtime's `WatchScheduler` observes them. They never start the loop or push text.
  (Writing one small state file bends the "MCP is read-only" rule the least — v1's
  `vlc_*` already spawn VLC.)
- **Gating, honestly.** The interjection gate is modeled on
  `IdleScheduler::should_fire` (cooldown + quiet hours) plus the session's
  `muted`/`consent` flags — **not** the companion `Outbox` (whose
  daily-cap/situation-table/ledger rhythm is the wrong fit for per-scene events).
  `earned_permission::check` may be consulted once for "is proactivity allowed at all."

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
   (when `ffmpeg` is present: `-map 0:s:0`) → otherwise `NoTranscript`.

Whisper audio-transcription fallback is **deferred** (it adds a heavy new dependency —
a whisper binary/endpoint — and downloads audio). v2 returns `NoTranscript` with a
friendly message when no captions exist; Whisper is a future enhancement (§7).

Caching: `~/.mur/runtime/transcripts/<sha256(source)>.json`. Cache hit skips
acquisition. Caveat: auto-generated captions can change and local files can be edited —
cache is best-effort, not invalidated automatically. Subtitle language preference is a
fallback chain (e.g. `zh-Hant → zh-Hans → en → auto`).

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
- **Last-source persistence (required for "analyze current video").** VLC's
  `status.xml` does not expose a usable source URI — and for YouTube it would only
  report a title, from which the watch URL (needed for `&t=` links and yt-dlp) cannot
  be reconstructed. Therefore `vlc::open(source)` must **persist the original source
  string** (e.g. `last_source` in `~/.mur/runtime/watch.json`). `video_analyze` with no
  `source` reads `last_source`, not VLC status.

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

1. `TranscriptService.get(source)` (source omitted ⇒ `last_source` from `watch.json`,
   §4.2 — not VLC status).
2. **Chunk:** chapter-aligned if chapters exist, else 60s time-windows with overlap;
   timestamps preserved.
3. **Map:** local model, **temperature 0**, per chunk → key points + notable
   timestamps.
4. **Reduce:** structured output → **topic / key-point list (with timestamps) / key
   moments / conclusion & analysis**.

Modes (`mode` param):

- `summary` (default) — overview + key points.
- `conclusions` — analysis / takeaways / "結論".
- `qa` — requires `focus` (a question). v2 keeps this **deliberately shallow**: BM25
  keyword scoring over cue text → top-k cues → answer with cited timestamps. It does
  **not** reuse the pattern hybrid-retrieve pipeline (that is tuned for patterns, not
  arbitrary transcripts). Embedding-based RAG is deferred (§7). If shallow `qa` proves
  weak in testing, ship `summary`/`conclusions` first and move `qa` to a follow-up.

Output: structured `serde` value **and** rendered markdown — YouTube uses `&t=` deep
links, local files use `[mm:ss]`. Language follows user locale.

Surface:

- **MCP tool:** `video_analyze(source?, mode?, focus?)`.
- **Skill manifest:** `mur-core/src/skills/video_analyze.yaml` — teaches when to use
  ("總結這支影片 / 重點 / 結論 / 他在講什麼"), to cite timestamps, and the privacy note
  (§6). Triggers: keyword (zh + en) + manual.

## 6. Pillar B — Proactive Co-Watching (runtime-only)

**Read §3 first:** this entire pillar lives in `mur-agent-runtime` and only works when
MuR runs as a murmur agent runtime with a delivery channel. MCP tools here are state
flags, not the engine.

### 6.1 `WatchSession` (runtime-persisted state)

The single source of truth, shared by the MCP `watch_*` tools (writers) and the
runtime `WatchScheduler` (reader). Persisted at `~/.mur/runtime/watch.json` (same
atomic temp+rename pattern as `vlc.json`):

```
struct WatchSession {
  active: bool, last_source: String, muted: bool,
  last_interjection_ms: i64, last_scene_phash: u64,
  consent: Consent,            // Unasked | Granted | Declined
}
```

(`last_source` is also written by `vlc::open`, §4.2, so Pillar A can reuse it.)

### 6.2 `WatchScheduler` (push; sibling of `IdleScheduler`, in the runtime)

Spawned by the supervisor (like `IdleScheduler`), holding `Arc<TaskRunner>`. Every
`scene_poll_secs` (default ~6s), if `session.active` and VLC `state == playing`:

1. Snapshot the current frame → compute a **perceptual hash (dHash)** (requires the
   `image` crate — a **new dependency**, §6.4).
2. Hamming distance vs `last_scene_phash`.
3. Gate (modeled on `should_fire`): distance ≥ `scene_change_threshold` **and**
   `(now − last_interjection_ms) ≥ interjection_cooldown_secs` **and** `!muted` **and**
   `consent == Granted` **and** not in quiet hours.
4. On pass: inject a turn via `runner.run_sync(TaskSpec{..})` — frame data-url +
   `window(now, ±span)` transcript + "briefly narrate this turn." The agent's output is
   **delivered through the companion channel (voice/notification)** — without a delivery
   channel the interjection is silent, so Pillar B requires one (§3). Update
   `last_interjection_ms` and `last_scene_phash`.
5. If `consent == Unasked` on the first large change: inject a **one-time consent ask**
   instead ("要我在劇情轉折時插話嗎？說「噓」可隨時靜音"). Record `Granted`/`Declined`.

Notes: `av-scenechange` is heavier (needs a decode stream); snapshot + dHash is what v2
uses. The dHash threshold mis-fires on fades/pans (gradual) vs hard cuts (large jumps)
— it needs empirical tuning, and the **cooldown is the real safety net** against
over-talking, not the threshold.

### 6.3 Snapshot lifecycle (cost control)

Polling every ~6s for a 2-hour film would otherwise leave ~1200 PNGs in
`snapshot_dir`, and v1's `newest_file` scans the whole dir each call (O(n), worsening).
The `WatchScheduler` therefore **overwrites a single fixed snapshot path** (or rotates
with a tiny cap) per session, so capture is O(1) in disk and scan. (`scene_explain`'s
on-demand path is unaffected.)

### 6.4 Mute / "噓", session control, dependencies

- **MCP tools (state flags only):** `watch_start(source)`, `watch_stop`,
  `watch_mute`, `watch_status`. They write `watch.json`; the runtime `WatchScheduler`
  reacts. They do **not** start the loop or emit narration (§3).
- **Skill manifest:** `mur-core/src/skills/watch_together.yaml` — teaches session
  semantics and etiquette (ask consent before interjecting, "噓" = mute immediately,
  decline DRM). `vlc-control` and `scene-explain` manifests stay as-is.
- **New dependency:** `image` (PNG decode for dHash). Flagged for the plan.

## 7. Out of Scope (YAGNI v2)

- **Idle auto-pause/resume.** Deferred: the only available "idle" signal
  (`TaskRunner::last_activity_at`) measures *agent-task* idleness, not whether the
  *user* stepped away from the movie — during viewing the user normally issues no agent
  commands, so it would mis-pause. This needs a real presence signal (companion
  presence) before it can be built; revisit then.
- **Whisper audio-transcription fallback** (heavy new dependency; downloads audio).
- **Embedding-based RAG** over transcripts (map-reduce suffices for v2; `qa` stays
  shallow BM25).
- `factcheck` mode.
- Continuous live-commentator narration (Pillar B is gentle/rate-limited only).
- A Hub GUI watch panel (CLI/MCP/companion layer only this round).
- Bundling yt-dlp (licensing/size — document install instead).
- Multi-frame temporal "frame-grid" reasoning; non-VLC players.

## 8. Testing

- **Unit:** json3 parse; srt/vtt parse; `Cue::window`; `chunks` (window + overlap +
  chapter alignment); dHash + Hamming distance; watch-gate (cooldown / consent / mute /
  quiet-hours, `should_fire`-style); `last_source` persist+load roundtrip; `MediaError`
  → message mapping; manifest validity.
- **Integration:** json3 fixture → `Transcript`; mock LLM map-reduce → structured
  output with timestamps; `video_analyze` with no source reads `last_source`; synthetic
  phash sequence + cooldown → exactly one injection (and consent-ask on first change);
  snapshot path is overwritten, not accumulated.
- **Manual E2E (runtime):** analyze a YouTube link → zh-TW conclusions with `&t=`
  links; run MuR as a runtime agent, start a session → scene change → consent ask →
  interjection delivered via voice; "噓" silences; DRM declined gracefully; yt-dlp
  missing → friendly decline but playback still works.

## 9. Affected Components & Plan Split

Two implementation plans share one spec:

- **Plan A — Foundation + analysis:** `TranscriptService`, `SourceResolver`,
  `MediaError`, `last_source` persistence; `video_analyze` tool + `video-analyze`
  skill. Files: `mur-core/src/cmd/media/{transcript.rs,resolve.rs,error.rs}`,
  `mur-core/src/cmd/media/vlc.rs` (persist `last_source` in `open`),
  `mur-mcp-server/src/tools.rs`, `mur-core/src/skills/video_analyze.yaml`,
  `mur-core/src/cmd/sync_cmd.rs` (register skill).
- **Plan B — Proactive co-watching (runtime):** `WatchSession` + `watch.json`,
  `watch_*` state-flag tools, `watch-together` skill, and the `WatchScheduler` itself.
  Files: `mur-core/src/cmd/media/watch.rs` (session state + dHash + snapshot lifecycle;
  new `image` dep), `mur-agent-runtime/src/watch_scheduler.rs` + `supervisor.rs`
  wiring + companion voice/notification delivery, `mur-mcp-server/src/tools.rs`,
  `mur-core/src/skills/watch_together.yaml`, `mur-core/src/cmd/sync_cmd.rs`.

Rationale for the split: Plan A is offline/batch, host-agnostic, and self-contained;
Plan B is push, **runtime-only**, and depends on both the shared foundation and a
running supervisor + delivery channel. They can be implemented, tested, and shipped
independently — and Plan A delivers value even if Plan B never ships.
