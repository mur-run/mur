# Unified Chat — Phase 2 prerequisites and carried-forward defects

**Date:** 2026-08-17
**Source:** execution of `docs/superpowers/plans/2026-08-16-unified-chat-phase1-model-and-contracts.md`
**Spec:** `docs/superpowers/specs/2026-08-16-unified-chat-redesign-design.md`
**Branch:** `feat/unified-chat-phase1` (20 commits off `78736ae7`)

Phase 1 shipped the model and the three read contracts. It deliberately ships
**no consumers** — nothing outside `mur-channel`'s own tests calls
`list_conversations`, `list_runs`, or `search`. Several defects below are
dormant *only* because of that, and become live the moment Phase 2 wires a
surface. Read this before writing the Phase 2 plan.

## Must fix before the first consumer lands

1. **`last_read_seq` still defaults to `0`** — the same 0-indexed collision
   `last_seq` had before it got a `-1` sentinel. A channel whose genuine first
   event is an *inbound* message (seq 0) computes `0 > 0 == false` and shows as
   already-read. Reachable for agent-opened channels: proactive companion
   messages and agent-authored openers. Fix in **two** places, not one: the
   column default in `mur-channel/src/index.rs`, and `rebuild_from`'s
   `WHERE last_read_seq > 0` watermark snapshot. Needs a test with an
   agent-authored seq 0 — the current unread suite systematically opens every
   fixture with a human message, which is exactly what conceals this.

2. **Cross-process out-of-order folds are silently dropped.**
   `ChannelIndex::record_event`'s guard is `?2 > last_seq`, which is idempotent
   for same-process reruns but assumes ordering that is not enforced:
   `ChannelStore::append_event` releases `events.lock` before the fold happens.
   Two processes writing one channel — the v3d-2 `channel/delegate` path does
   exactly this — can fold out of order, and the lower seq is then dropped
   entirely: no count, no preview, no FTS row, so search can never find that
   message. Fix by deduplicating on `(channel_id, seq)` membership rather than
   on `>`, so ordering stops mattering. `mur internals reindex` recovers
   existing damage.

3. **`hitl_pending` is a second, less accurate implementation of a fact that
   already has one.** `record_event` sets the flag on any `HitlRequest` and
   clears it on any `HitlResponse` with no id matching; the authoritative
   version in `mur-core/src/cmd/channel.rs` tracks `hitl_id` against a
   responded set. With two gates open in one channel, answering either clears
   the flag while a gate is still pending. Converge before Home's "Needs You"
   reads it — this is the same-fact-two-interfaces pathology behind #917/#940.

4. **Empty-title fallback on mobile and Hub.** `create_for_agent` now writes an
   empty title, filled from the first human message. For any channel where the
   agent speaks first, it stays empty permanently. `ChannelListView.swift`
   renders a blank row; `RecentActivity.tsx` / `NowRunning.tsx` fall back to a
   raw UUID stub — the exact symptom the spec exists to remove. Also note the
   ~268 existing channels keep `"chat with mur"` until an explicit title
   backfill runs.

5. **Migrate `latest_for_agent`'s callers.** `list_conversations` is not
   susceptible to a recent fleet run shadowing an agent's real chat;
   `latest_for_agent` still is, and remains the live resume path for
   `mur-core/src/mobile.rs` and `mur-hub-gui/src-tauri/src/chat.rs`.

## Should fix during Phase 2

- **Index health check.** A rebuild interrupted by process kill leaves all
  channels on SQL defaults permanently: `just_migrated` never fires again and
  the only signal is a `tracing::warn!` invisible without `RUST_LOG`.
  `mur internals reindex` is a working manual recovery, but nothing tells the
  user it exists or detects that they need it. Add a cheap plausibility check
  (rows exist but every `msg_count` is 0 while manifests have events).
- **`search()` degraded mode.** Spec §14 requires title/participant results
  with a notice when content search is unavailable; it currently returns `Err`
  wholesale.
- **Cross-contract inconsistencies.** `search()` skips `msg_count == 0` but
  `list_runs()` does not, so a titled fleet run with no messages appears in
  Work yet is unfindable. `search()` matches bodies via tokenised FTS5 `MATCH`
  but titles via `to_lowercase().contains()` — two matching semantics in one
  query. `"conversation"` appears as a bare literal in three places instead of
  round-tripping `ChannelPurpose::Conversation` (CLAUDE.md rule 1).
  `latest_for_agent` scans 1000 rows while the contracts use 2000.
- **`mur-core/src/server/governance.rs` bypasses `ChannelService`,** appending
  via `ChannelStore` directly, so fleet governance receipts never fold into the
  read model. Lower risk than it sounds (non-`Message` events, so counts and
  preview are unaffected); the real cost is FTS invisibility.
- **`inbound_seqs` grows without bound** — read seqs are never pruned and
  `list_conversations` parses the whole array per channel per render. Sub-ms at
  268 channels. Prune entries `<= last_read_seq` on `mark_read`.
- **Migration lock.** Nothing guards the `ALTER TABLE` loop *across* processes;
  simultaneous first-open after upgrade is only probabilistically safe via WAL
  + `busy_timeout`. `BEGIN IMMEDIATE` on the rebuild narrows it; a full fix
  needs an explicit lock.
- **Extract `mur-channel/src/service.rs`'s test module.** The file is ~1360
  lines, but only ~620 are production code. Per CLAUDE.md the split is pure
  code movement and belongs in its own PR.

## Tests that do not currently discriminate

Four were caught and rewritten with negative controls during Phase 1. These
remain, and each is annotated in place:

- `a_new_agent_message_after_reading_is_unread_again` — its `mark_read` call was
  removed because it set the watermark to its own default; the test cannot
  discriminate until the `-1` sentinel lands (item 1 above).
- `search_matches_titles_as_well_as_bodies` — its fixture text is both title and
  body, so the body match satisfies it alone. The title path *is* covered by
  `a_title_only_match_reports_no_event_to_scroll_to`.
- The whole unread suite opens every fixture with a human message at seq 0, so
  no inbound message is ever at seq 0 — the systematic hole concealing item 1.
- `the_watermark_never_moves_backwards` passes `mark_read(3)` to a channel whose
  highest real seq is 2. The assertion is load-bearing; the argument is a
  count where a seq belongs.
- `SearchScope::Runs` is never exercised — `scope_filters_the_result_set` tests
  only the `Conversations` arm.

## Deliberately not doing

- Pagination past `SUMMARY_SCAN_LIMIT = 2000` (7× headroom at current scale).
- Deduplicating identical-participant group conversations under `active_only`
  (a product question, not a defect).
- Guarding against a legacy title beginning `"workflow: "` inferring
  `WorkflowRun` — unreachable, since legacy titles are `"chat with {agent}"`
  and `derived_title` requires an empty title.
- Sorting `list_ids()` for deterministic `--limit` batches in
  `backfill-purpose` (idempotent and re-runnable).
- Counting corrupt manifests skipped during backfill.
