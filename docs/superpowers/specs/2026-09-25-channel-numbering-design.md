# Channel Numbering & Switching — Design

Date: 2026-09-25
Status: design approved (pending implementation plan)

## Problem

Channels are only identifiable by full id. In `murmur` the footer shows a
channel with no handle to switch by, and in MUR Hub the channel column is too
narrow to read names and cannot be resized.

## Decisions

1. **Dual handle.** Every channel gets a stable ordinal *and* keeps its short
   id prefix (8 hex chars, e.g. `01a0d420`). Both are displayed; both resolve.
2. **Ordinal assignment.** Assigned by channel creation time, monotonic, never
   recycled when a channel is left or closed — the number a user memorizes
   stays stable across sessions.
3. **Display.** Ordinal + short id shown together in:
   - the `murmur` footer channel indicator;
   - both channel labels in MUR Hub (list row and header, per screenshot).
4. **Switching.** `/channels <n>` and `/channels <short-id>` both switch.
   `--follow` / `-f` keeps its current semantics on either form.
5. **Hub column width.** The channel column becomes user-resizable via a drag
   handle (not merely widened); the chosen width persists.

## Open for the plan

- Where the ordinal is persisted (channel store vs. derived index) and how it
  behaves for imported/synced channels.
- Ambiguity rule if a short-id prefix is also a valid ordinal.
- Persistence location for the Hub column width.

## Verification

- CLI: `/channels 2`, `/channels 01a0d420`, and the `--follow` variants resolve
  to the same channel; ordinals survive a restart and a channel deletion.
- Hub: drag resize persists across app restart; both labels show ordinal + id.
