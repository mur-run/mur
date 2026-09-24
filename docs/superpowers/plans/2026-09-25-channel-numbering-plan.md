# Channel Numbering & Switching — Implementation Plan

Design: `docs/superpowers/specs/2026-09-25-channel-numbering-design.md`
Status: ready for implementation

## Decisions that were open in the design

1. **Ordinal is persisted, not derived.** New `ordinal INTEGER` column on the
   `channels` table in `mur-channel/src/index.rs` (the SQLite read model),
   `UNIQUE`, assigned as `MAX(ordinal) + 1` on first `upsert` of an id. Derived
   ranks cannot satisfy "never recycled on delete" — a deleted channel's number
   must stay burned, which only a stored value gives.
   - Migration: existing rows backfilled `ORDER BY created_at ASC, rowid ASC`
     in the same `ALTER TABLE` migration step used by prior columns; runs inside
     the existing `open()` migration path (`just_migrated` already signals it).
   - Imported/synced channels: treated like any first-seen id — they get the
     next local ordinal at upsert time. Ordinals are **local view state**, never
     part of a signed event, so two machines may number differently. State this
     in the `ChannelRow` doc comment so nobody later tries to sync it.
2. **Ambiguity rule.** A `/channels` argument is resolved by shape, not by
   trying both:
   - all-decimal (`^[0-9]+$`) → ordinal lookup;
   - otherwise → case-insensitive **id prefix**, minimum 4 chars, must match
     exactly one channel; 0 matches → `no channel <arg>`, >1 → list candidates.
   - An all-decimal token is therefore *never* read as an id prefix. Short ids
     are 8 hex chars and an all-digit one is possible, so document the escape:
     the full id always resolves as a prefix of itself... which is also all
     digits. Escape hatch: a leading `#` forces id-prefix mode (`/channels
     #01a0d420`, `/channels #12345678`). No `#` = the common path.
3. **Hub column width persistence.** `localStorage` key
   `mur.hub.chatRailWidth` (px integer), clamped to the CSS min/max. Same
   mechanism the UI already uses for view-local preferences; no Tauri state or
   config file for a cosmetic width.

## Work items

### A. Channel index: the ordinal (mur-channel)

- `index.rs`: add `ordinal: i64` to `ChannelRow`; schema + migration + backfill
  as above; assign in `upsert`; include in `list()`'s SELECT; keep
  `rebuild_from` stable (rebuild must preserve existing ordinals — rebuild
  reads them from the surviving rows, not reassign).
- Tests (in-crate): assignment is monotonic; deleting the middle channel does
  not renumber the others and does not reuse its ordinal; backfill orders by
  `created_at`; rebuild is ordinal-preserving.

### B. CLI plumbing (mur-core)

- `cli/persist.rs`: `SessionInfo` and `ChannelMeta` each gain `ordinal: u64`;
  `list_recent` carries it through from `ChannelRow`; `Session::current()`
  fills it for the live channel.
- `cli/app/slash.rs`: `SlashCmd::Channels { n, follow }` → `target:
  Option<ChannelRef>, follow: bool` where
  `enum ChannelRef { Ordinal(u64), IdPrefix(String) }`. Parse per decision 2.
  Keep `--follow` / `-f` semantics untouched.
- `cli/slash_cmds.rs`: replace both `recent.get(n.wrapping_sub(1))` lookups
  (switch path and follow path) with one `resolve(&recent, &ChannelRef)` helper
  returning `Result<&SessionInfo, ResolveErr>`; render the three error shapes
  (not found / ambiguous / too-short prefix) as system lines. The listing
  output prints `ordinal · short-id · N turns · preview` instead of the loop
  index, so the number shown is the number that works.
- `cli/ui/status.rs`: footer chip becomes `⏵ 2 · 01a0d420`. Update the existing
  chip test; it currently asserts id-only.
- `/help` text and the `SlashCmd::Channels` doc comment: document both forms.
- Tests: `app/tests/state.rs` parse cases (`2`, `01a0d420`, `#12345678`,
  `2 --follow`, `--follow` alone); `help_coverage_tests.rs` match arm update;
  resolver unit tests for ambiguous and short prefixes.

### C. MUR Hub (mur-hub-gui)

- `src-tauri/src/work.rs`: `ChannelSummary` gains `ordinal: u64` from
  `ChannelRow`; mirror it in `ui/src/work/types.ts`.
- `ChatChannelRail.tsx`: row shows the ordinal in place of the bare `#` glyph
  (`#2`), with the 8-char short id as a dim trailing span; `title` attribute
  carries full title + id for hover.
- `RecentActivity.tsx` / `NowRunning.tsx` (the two screenshot labels): prefix
  the label with the ordinal, same shape.
- Resizable rail: drag handle on the right edge of `.cw-rail` in
  `AgentChatWindow.tsx`; pointer-event based (pointerdown/move/up with capture),
  width state in a `useChatRailWidth` hook that reads/writes localStorage and
  clamps to `[160, 420]`; CSS `width` driven by inline style, `min-width` kept
  as the floor. Double-click on the handle resets to the 200px default.
- Tests: hook test for clamp + persistence round-trip; rail render test that the
  ordinal and short id both appear.

### D. Docs

- `README.md` `/channels` line, docs site (`core`) and product page per the
  `update-docs` skill. One line each: both handles switch; the number is stable.

## Order of execution

A → B → C → D. A is a schema change and everything downstream reads the column,
so it lands first and alone (migration test included). B and C are independent
of each other once A is in. D last, after the surfaces are final.

## Verification

Per the design's verification section, plus:

```bash
cargo clippy --all --all-targets --no-deps --locked -- -D warnings
cargo fmt --all -- --check
cargo test -p mur-channel -p mur-core
```

Manual: `murmur` → `/channels` (numbers shown), `/channels 2`, `/channels
01a0d420`, `/channels 2 --follow`, `/channels --follow`; restart and confirm the
same numbers; delete a channel and confirm no renumbering. Hub: drag the rail,
restart the app, width persists; both labels show `#N` + short id.
