# Murmur Panel — Companion Window Design

**Date:** 2026-07-05
**Status:** Approved (brainstormed with David)

## Problem

`murmur` (`mur agent cli`) is a terminal chat TUI. While working with an
agent, the user has no ambient surface showing: where the agent is
working (cwd, repo, branches, worktrees), what is running (workflows,
tasks, HITL gates), what the agent produced (plans, reports, frontend
prototypes — HTML that a terminal cannot render), and agent
notifications. Everything competes with the chat transcript for the
same 80 columns.

## Decisions (brainstorm outcomes)

- **Host:** a window of the MUR Hub app (Tauri). Reuses the existing
  floating-window precedent (`pet/`), `geometry.rs`, `notif.rs`,
  `hitl.rs`, and `mur-gui-core` (`EventBus`, watcher patterns). Users
  without the Hub simply don't get the feature.
- **Name:** **Panel** (`/panel`, `panel_bridge`, `PanelWindow`) — NOT
  "Companion", which is already the voice + proactive-messaging
  subsystem.
- **Positioning:** snap-once next to the murmur terminal window at
  open, then a free always-on-top floating window. A clean
  `reposition(target: Option<WindowBounds>)` interface is kept so
  live-follow (AXObserver) can be added later by attaching an observer
  and handling the edge cases (multiple windows, Spaces, minimize).
- **Terminal environment:** user runs murmur directly in iTerm2 /
  Terminal.app / WezTerm / kitty / Ghostty — not tmux. A tmux-pane
  companion was rejected.
- **Transport (Approach A):** murmur hosts a per-session Unix socket;
  the Hub discovers sessions via a watched directory and connects.
  Rejected: pure file-based bridging (polling latency, two watchers,
  stale-file cleanup, awkward for streaming) and daemon-brokered
  transport (murmur chat does not otherwise require the daemon; no new
  failure point).
- **Click semantics:** clicking a suggestion in the Panel only
  **inserts** the command into murmur's input box; the user presses
  Enter. Never auto-execute (fail-closed).
- **Preview scope:** all three modes, phased — (1) files the agent
  wrote (HTML / Markdown, auto-reload on change), (2) frontend
  dev-server URL, (3) live rendering of the agent's streaming output
  (last, gated).

## Architecture

```
┌───────────────┐  hello/state/panel/preview/stream   ┌──────────────────┐
│ murmur (TUI)  │ ──── unix socket (JSON lines) ────▶ │ mur-gui-core     │
│ cli/panel.rs  │ ◀──────── insert{text} ──────────── │ panel_bridge     │
└──────┬────────┘                                     └───────┬──────────┘
       │ writes/removes                                       │ EventBus →
       ▼                                                      ▼ Tauri events
~/.mur/runtime/murmur/<session>.json   ◀── dir watcher ── mur-hub-gui panel.rs
  (pid, sock, agent, cwd,                                     │
   terminal hint)                                             ▼
                                                     PanelWindow (React)
                                                     Info│Activities│Preview│Notif
```

### Components

| Component | Location | Responsibility |
|---|---|---|
| Frame types | `mur-common/src/panel.rs` | Pure types: bidirectional frame enums + `PANEL_PROTO_VERSION` (mirrors the `mur-common::mobile` ClientFrame/ServerFrame precedent) |
| murmur side | `mur-core/src/cmd/agent/cli/panel.rs` | Session file write/cleanup, Unix socket server task, `/panel` command handling, applying `insert` to the input box |
| Bridge | `mur-gui-core/src/panel_bridge/` (`mod.rs`, `discovery.rs`, `client.rs`) | Watch the session dir, connect/reconnect socket clients, republish frames on `EventBus` |
| Hub backend | `mur-hub-gui/src-tauri/src/panel.rs` | Create/focus the PanelWindow, snap-once positioning, `panel_repo_info(cwd)`, recommendations query, forwarding `insert` |
| Hub frontend | `PanelWindow` (React) | Four tabs: Information / Activities / Preview / Notifications; session dropdown in the header |

Each component is consumable without reading its internals: frame types
are the only shared contract; the bridge exposes only EventBus events;
the murmur side exposes only the socket.

## Transport & Lifecycle

- On TUI start, murmur writes
  `~/.mur/runtime/murmur/<session-id>.json` — `{pid, sock, agent, cwd,
  terminal: {program, pid}, proto_version, started}` — and listens on
  the Unix socket (mode 0600) in the same directory. Both are removed
  on clean exit.
- The Hub watches `~/.mur/runtime/murmur/` (reusing the existing watcher
  pattern). New live session → connect the socket. A scanner reaps
  stale session files whose pid fails `kill -0`.
- First `/panel` invocation when the Hub is not running: murmur runs
  `open -g -a "MUR Hub"`; the Hub discovers the session on startup and
  connects. If the Hub is not installed, murmur prints a one-line
  notice.
- Frames, murmur → Hub: `hello` (session metadata + terminal hint),
  `state {cwd, agent}` (on change), `panel {focus}` (from `/panel`),
  `preview {kind, target}`, `stream {delta}` (phase 4, gated), `bye`.
  Hub → murmur: `insert {text}`. All frames carry nothing executable;
  `insert` only mutates the input-box content.
- Version negotiation: `hello.proto_version`; the Hub ignores unknown
  frame variants (serde `#[serde(other)]`-style tolerance) so old TUIs
  keep working against a newer Hub.
- Socket EOF = session over: the PanelWindow shows "session ended" and
  closes shortly after. Hub restart re-discovers live sessions from the
  directory — no state is lost because panel data is recomputed from
  disk and events.
- Multiple murmur sessions: a single PanelWindow bound to one session;
  a header dropdown lists live sessions; the most recent `hello` is the
  default binding.

## Window Positioning

Snap-once, then free-floating always-on-top:

1. `hello.terminal` carries `TERM_PROGRAM` and the terminal app pid
   (walked up murmur's ppid chain).
2. The Hub calls `CGWindowListCopyWindowInfo`, filters by owner pid,
   and takes that app's frontmost window bounds. Window **bounds**
   require no Accessibility or Screen Recording permission (only window
   titles do).
3. `reposition(target: Option<WindowBounds>)` places the PanelWindow at
   the terminal's right edge (screen-clamped); `None` falls back to the
   right edge of the main screen.
4. Future live-follow attaches an AXObserver that re-invokes
   `reposition` on move/resize events; `reposition` itself never needs
   to change.

## Panels (data sourced Hub-side; murmur stays thin)

- **Information** — repo root, current branch, branch list, worktrees,
  dirty status: the Hub shells out to `git` against the session's cwd,
  refreshed on `state` changes and window focus. Recommended actions
  (skills / workflows relevant to the current context): served by a new
  hidden command `mur internals recommend --cwd <path> --json` that
  reuses the retrieve pipeline (`score_and_rank_generic`) — added
  because no existing surface emits this as JSON. Clicking a
  recommendation sends `insert` with the ready-to-edit command.
- **Activities** — running workflow runs, pending HITL gates, queued
  agent jobs: reuse the data sources behind the Hub's existing `work.rs`
  view, filtered to the bound session's agent.
- **Preview** — a sandboxed iframe in the webview. Modes, phased:
  file (`.md` rendered with the Hub's existing markdown component,
  `.html` loaded directly; a `notify` file-watch reloads on change),
  dev-server URL (restricted to localhost/127.0.0.1), and live
  agent-stream rendering (murmur forwards `stream {delta}`; gated,
  built last). Target set by `/panel preview <path|url>` or by clicking
  a produced-file notification.
- **Notifications** — the existing `notif.rs` / EventBus / HITL event
  streams filtered to the bound agent. Clicking a HITL item inserts the
  corresponding approve command into murmur's input box (insert-only,
  consistent with the HITL flow).

## murmur Command Surface

`/panel [information|activities|preview|notifications] [target]`

- No argument: open/focus the PanelWindow (launching the Hub if
  needed).
- Subcommand: switch the focused tab (sends `panel {focus}`).
- `preview` takes an optional `<path|url>` target.
- Autocomplete follows the existing `/mcp` subcommand pattern in
  `complete.rs`.

## Phasing

1. **P1 — end-to-end skeleton:** frame types, murmur socket server +
   session file, discovery + bridge, empty four-tab PanelWindow,
   snap-once positioning, `/panel`, `insert` round-trip.
2. **P2 — data panels:** Information (git), Notifications, Activities
   (all Hub-side data).
3. **P3 — Preview:** file mode + dev-server URL + file-watch reload.
4. **P4 — recommendations + live stream render:** `mur internals
   recommend`, `stream {delta}` forwarding (experimental, gated).
5. **Future:** AXObserver live-follow via `reposition`.

Each phase lands independently; P1 is demoable (click a test button in
the Panel → text appears in murmur's input box).

## Security & Error Handling

- Socket mode 0600 under `~/.mur/runtime/murmur/`; local user only.
- Insert-only: no frame can trigger execution; the user always confirms
  in the terminal (fail-closed, consistent with HITL).
- Preview URLs restricted to localhost; iframe sandboxed (no
  top-navigation, no downloads).
- Stale sessions reaped by pid liveness check; socket errors mark the
  session dead rather than crashing the bridge; murmur tolerates the
  Hub connecting/disconnecting at any time (frames are fire-and-forget
  from its side).

## Testing

- `mur-common` frame round-trip serde tests (including
  unknown-variant tolerance).
- murmur socket server integration test: connect → `hello` → send
  `insert` → assert input-box content; session file created and
  removed.
- Snap geometry unit tests (clamping, fallback).
- `mur internals recommend` JSON output test.
- Hub window behavior (open/focus/session-ended) verified manually.

## Out of Scope

- tmux/multiplexer pane variant.
- Live window-follow (interface reserved only).
- Remote/SSH murmur sessions (socket is local-only by design).
- Auto-execution of any Panel action.
