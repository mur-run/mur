# Native Media Output Adapter (Design)

Date: 2026-07-14
Status: **DEFERRED** — backlog only, no implementation planned. YAGNI: no user
is waiting on this; deep-research just went end-to-end (#663–#670) and its value
is *credibility*, not content re-production. Revisit only if real demand appears.

## 1. Goal

Optionally turn a **verified** deep-research report into consumer media formats:

- **Podcast** — two-voice dialogue script → TTS audio (commute listening).
- **Quiz** — Markdown questions + answer key (self-test comprehension).
- **Mindmap** — structured JSON knowledge tree (render to a diagram downstream).

The adapter runs **after** synthesis, taking the already-cited, already-verified
report as its only input. It never re-does research and never re-verifies.

## 2. Motivating comparison — why NOT wrap `notebooklm-py`

`notebooklm-py` (CLI wrapper around Google NotebookLM) produces the same three
media formats for free, and prompted this spec. We explicitly reject wrapping it:

1. **Re-uploads to Google.** It would send the freshly sandboxed,
   egress-governed report back out to Google's cloud — directly negating the
   deep-research choke point (#689 SSRF-screen + IP-pin, #691 proxy-only
   loopback). A privacy-first pipeline must not open a "ship the result to
   Google" hole at the last mile.
2. **Fragile unofficial API.** It drives Chrome automation against NotebookLM's
   web frontend; any Google UI change breaks it (same brittleness class as our
   browser-automation gotchas).
3. **Nothing here needs NotebookLM.** All three outputs are plain LLM tasks —
   a dialogue-script prompt + TTS, a quiz prompt, a structured-output prompt.

So the value question is never "integrate notebooklm-py" — it is "does MUR want
native media outputs at all." This spec captures the native answer, deferred.

## 3. Design (when/if built)

Zero new subsystems. One optional post-synthesis stage:

```
deep-research fleet → verified report (cited)
                          │
                          ▼  (opt-in only, e.g. --media podcast,quiz)
                   media output adapter
                     ├─ podcast: LLM dialogue script → existing TTS stack
                     ├─ quiz:    LLM prompt → report.quiz.md
                     └─ mindmap: LLM structured output → report.mindmap.json
```

- **No new dependency.** TTS reuses the existing voice stack (companion voice /
  murmurd / Whisper packaging). Quiz + mindmap are prompts over the report text.
- **Inherits egress governance.** The adapter is a normal fleet/agent stage, so
  it stays inside the same sandbox + proxy-only egress — data never leaves.
- **Opt-in, off by default.** No media generated unless the user asks
  (`--media <formats>`). Deep-research's default output stays the cited report.
- **Input is the report, not the sources.** The adapter never touches raw
  fetched pages, so it cannot reintroduce unverified claims.

## 4. Non-goals

- Not matching NotebookLM's polish or voice quality.
- No new audio pipeline — if the voice stack can't do two-voice TTS acceptably,
  the podcast format is dropped, not rebuilt.
- No mindmap *rendering* — emit JSON; rendering is a downstream/GUI concern.

## 5. Decision

**Do not build now.** If revisited: build native (Section 3), never wrap
`notebooklm-py`. The only thing worth borrowing from that tool is the UX ideal
(one line → research → cited report), which deep-research already delivers.
