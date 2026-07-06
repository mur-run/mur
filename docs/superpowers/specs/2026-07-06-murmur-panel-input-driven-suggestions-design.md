# murmur Panel: Input-Driven Dynamic Suggestions (`PanelFrame::InputChanged`)

**Status:** Proposed
**Date:** 2026-07-06
**Depends on:** murmur Panel P2–P5 (shipped), panel bridge protocol (`mur-common/src/panel.rs`)

## 1. Problem

The Panel's Recommendations block (P4) ranks skills/workflows using only the
session's **cwd** (`mur-core/src/recommend.rs::cwd_query`), queried once on
Panel open and on the ~30s poll. It does not react to what the user is typing
in murmur's message input. Users expect the panel to update suggestions live
as they type — like Fig/Amazon Q autocomplete beside a terminal — while
keeping the existing insert-only safety model.

## 2. Prior art (deep-research, 2026-07-06; 23/25 claims verified 3-0)

- **Fig / Amazon Q CLI** is the proven blueprint for this exact split:
  a terminal-side layer (figterm) captures the live edit buffer and streams
  **typed, schema-defined IPC frames** (protobuf over local IPC) to a separate
  Rust desktop app (tao/wry webview) that renders suggestions, insert-only.
  Maps 1:1 to murmur → `PanelFrame` over Unix socket → Hub Panel window.
- **Debounce:** Algolia recommends **200 ms**; >300 ms degrades UX; too-short
  causes suggestion flicker. Expensive backends throttle harder (ServiceNow:
  1000 ms + 3-char minimum).
- **Ranking (Firefox urlbar / fre / Raycast):** adaptive query→picked history
  outranks everything; frecency with **exponential decay (~30-day half-life)**
  as the base score; exact/prefix matches above fuzzy; stable deterministic
  tie-breaking. (VS Code Quick Open meta-issue #27317 catalogs the pain when
  these are violated.)
- **Privacy (USENIX Security 2019, Monaco; VS Code #49161):** per-keystroke
  autocomplete network traffic enables remote keylogging via packet
  timing/size side channels **even under TLS**. Keystroke-derived data must
  never leave the machine.
- **Refuted:** request cancellation does not "eliminate" typeahead latency —
  it is a mitigation on top of debounce, not a substitute.

## 3. Design

### 3.1 Protocol — one new frame, murmur → Hub

```rust
// mur-common/src/panel.rs
PanelFrame::InputChanged {
    /// Debounced snapshot of the message input line (NOT per-keystroke deltas).
    text: String,
}
```

- Sent by murmur's TUI event loop after a **200 ms debounce** on input edits
  (timer reset on each keystroke; also fired on input cleared → `text: ""`).
- Snapshot semantics: only the latest buffer content is ever sent; intermediate
  states are coalesced. Cap `text` at 2,000 chars (truncate head, keep tail —
  the tail is what the user is currently typing).
- `PANEL_PROTO_VERSION` bump. Unknown frames are already tolerated on both
  sides (`decode_line` returns `None`), so old Hub + new murmur and vice versa
  degrade gracefully to cwd-only recommendations.
- Direction is unchanged: Hub → murmur remains `HubFrame::Insert` only.
  **Insert-only stays absolute** — no frame may execute anything.

### 3.2 Privacy rules (hard requirements)

1. `InputChanged` travels **only** over the local Unix-socket panel bridge.
   It must never be persisted (no channel event, no session recording entry)
   and never forwarded to any network path (relay, mobile sync, Hub live-tail).
2. Debounced snapshots, not keystroke deltas — the frame stream must not
   encode typing cadence.
3. Redact obvious secrets before sending: replace tokens matching
   `sk-[A-Za-z0-9]{16,}`, 32+ char hex/base64 runs, and `AKIA[A-Z0-9]{16}`
   with `[redacted]` (shared helper in `mur-common`, unit-tested).
4. Hub keeps only the latest snapshot per pid in memory (`PanelState`);
   dropped on `SessionDown`. Never logged at info level (trace-gated, redacted).

### 3.3 Hub-side retrieval

Extend `mur-core/src/recommend.rs`:

```rust
pub fn recommend_for_input(cwd: &Path, input: &str, limit: usize) -> Vec<Recommendation>
```

Query construction:
- `input.trim().len() < MIN_QUERY_CHARS` (**2**) → fall back to
  `recommend_for_cwd` (current behavior; empty input = today's panel).
- Otherwise query = input text; cwd terms appended as low-weight context
  (input words listed first — `score_and_rank_generic` is word-overlap based,
  so input dominates naturally).

Ranking tiers (top → bottom), then truncate to **5** (existing cap):

1. **Adaptive history:** exact `(normalized query prefix → previously picked
   suggestion)` hits. Stored per user in `~/.mur/panel/adaptive.yaml`;
   Firefox parameters: `use_count = use_count * 0.9 + 1` on pick (cap 10),
   decay `0.975/day`, expire after 90 days unused. Written when the user
   clicks a suggestion (Hub records query + picked name via `panel_insert`).
2. **Prefix/exact name matches** on skill/workflow names (case-insensitive),
   ordered by the existing retrieval score.
3. **Retrieval-ranked results:** existing `score_and_rank_generic` pipeline
   (its recency/effectiveness/decay weighting is already frecency-shaped;
   tier half-lives 14/90/365d stay as-is), floor **0.42** unchanged; workflows
   keep the word-overlap fallback.
4. Ties broken by name, ascending (stable, deterministic).

No semantic/LanceDB path in this phase (see §6).

### 3.4 Hub-side flow

- `panel/mod.rs::on_frame`: handle `InputChanged` → store snapshot in
  `PanelState` → emit `panel-input-changed { pid }` to the Panel webview.
- New Tauri command `panel_recommend_input(pid, cwd)` reads the stored
  snapshot and calls `recommend_for_input`. Retrieval is in-process and local,
  so no extra Hub-side debounce; the webview drops stale responses
  (per-pid request generation counter) so out-of-order results never render.
- `PanelWindow.tsx`: Recommendations block re-queries on `panel-input-changed`;
  keep the current list rendered until the new one arrives (no flash-to-empty);
  show a subtle "matching your input" caption when input-driven.
- `panel_insert` gains an optional `query` arg so a click records the adaptive
  history pair atomically with the insert.

### 3.5 murmur-side flow

In the TUI input handler (`mur-core/src/cmd/agent/cli/app.rs`): on any edit to
the message input, arm/reset a 200 ms `tokio` sleep; on expiry send
`PanelHandle::send(PanelFrame::InputChanged { text })` (already fire-and-forget,
drops when no Hub is connected — zero cost without a panel). Slash-command
lines (`/...`) are sent too — prefix matches on command names are useful — but
lines that fail secret redaction are sent redacted.

## 4. Parameters (single source: constants in `mur-common::panel`)

| Constant | Value | Rationale |
|---|---|---|
| `INPUT_DEBOUNCE_MS` | 200 | Algolia sweet spot; >300 ms degrades |
| `MIN_QUERY_CHARS` | 2 | below → cwd fallback |
| `INPUT_SNAPSHOT_MAX_CHARS` | 2000 | bound frame size |
| result cap | 5 (existing) | internal precedent |
| score floor | 0.42 (existing) | `retrieval.min_score` |
| adaptive decay / expiry | 0.975/day / 90 days | Firefox urlbar |

## 5. Testing

- `mur-common`: frame round-trip + unknown-frame forward-compat (extend the
  existing `frames_round_trip` pattern); redaction unit tests.
- `mur-core`: `recommend_for_input` tiering (adaptive > prefix > retrieval),
  short-query fallback, stable tie-break, fail-soft on missing stores.
- Hub: `follow_tick`-style pure-function test for the stale-response
  generation counter.
- Live verify: tmux + iTerm recipe from PR #639 tracing; confirm typing in
  murmur updates the panel within ~400 ms and clicking a suggestion inserts
  and records the adaptive pair.

## 6. Out of scope / future

- **Semantic (LanceDB) blending** — kick in at ≥3 chars with a longer
  debounce; blend weight is an open research question.
- **LLM-generated suggestions** (Warp AI-style) — cost/latency gating unclear;
  suggest-on-pause only, if ever.
- **Hash/prefix-class instead of raw text** in the frame — stricter privacy
  posture; unnecessary for local-socket threat model, revisit if the panel
  bridge ever crosses a machine boundary.
- Declarative completion specs for `mur` subcommands (Fig-style schema) —
  separate effort, would slot into ranking tier 2.

## 7. References

- Algolia autocomplete debouncing guide; ServiceNow typeahead tuning
- Firefox urlbar ranking (frecency + adaptive history);
  camdencheek/fre; Raycast `useFrecencySorting`
- VS Code Quick Open meta-issue microsoft/vscode#27317
- aws/amazon-q-developer-cli-autocomplete (figterm / proto IPC / fig_desktop)
- Monaco, *Remote keylogging via search-engine autocomplete traffic*,
  USENIX Security 2019; microsoft/vscode#49161 (Bing keystroke incident)
- Deep-research run `wf_f5994355-bb3` (2026-07-06): 22 sources, 25 claims
  verified 3-vote adversarial, 23 confirmed / 2 refuted
