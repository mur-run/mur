# murmur Panel P5 — Live Window-Follow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The PanelWindow follows the murmur terminal window as it moves/resizes — permission-free polling, not AXObserver.

**Architecture:** A single-slot background poll loop (Hub-side, `panel/follow.rs`): every 600 ms read the terminal's bounds via the existing `terminal_window_bounds()` (CGWindowList — no Accessibility permission) and re-invoke the existing `reposition()` when they change. Manual-drag detection pauses following (the panel never fights the user): each tick compares the panel's actual position with the position we last set; a mismatch means the user moved it → pause until re-pinned. Frontend: a pin toggle in the panel header; follow starts pinned (on) and restarts whenever the bound session changes.

**Decision (brainstormed with David, 2026-07-06):** polling chosen over the parent spec's reserved AXObserver direction — zero permissions beats sub-second latency for a companion panel; AXObserver remains the upgrade path if polling ever proves insufficient. This supersedes the "Future: AXObserver" line in `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md`.

**Tech Stack:** Rust (Tauri 2, std::thread poll loop), React/TS (pin toggle).

**Prerequisites:** none beyond P1 (independent of P2–P4; only shared frontend file is `PanelWindow.tsx`'s header block — land after P2 to avoid churn).

## Global Constraints

- No new permissions: bounds come from `CGWindowListCopyWindowInfo` only (`pos.rs` already documents this).
- Poll cadence 600 ms (`FOLLOW_INTERVAL_MS` constant), loop runs only while a follow target is set; non-macOS builds are a no-op (`terminal_window_bounds` is already `#[cfg]`-gated to return `None`).
- The panel must never yank itself out from under the user: any user-initiated move pauses following until the user re-pins.
- fmt/clippy green per commit; Hub check via `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml` (stub `ui/dist/index.html` if absent, never commit).

---

### Task 1: Follow loop (`panel/follow.rs`)

**Files:**
- Create: `mur-hub-gui/src-tauri/src/panel/follow.rs`
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs` (`pub mod follow;`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (`.manage(panel::follow::FollowState::default())` + register `panel_follow`)
- Modify: `mur-hub-gui/src-tauri/capabilities/panel.json` (allow `panel_follow`)

**Interfaces:**
- Consumes: `crate::panel::pos::{reposition, terminal_window_bounds}` (`reposition(win: &WebviewWindow, target: Option<Rect>)`, `terminal_window_bounds(win, term_program) -> Option<Rect>`); `crate::geometry::Rect { x: i32, y: i32, w: i32, h: i32 }`; the panel window label used by P1's `open_panel_window` (grep `panel/mod.rs` for the `WebviewWindow` label string — reuse the same constant).
- Produces:
  - `pub struct FollowState(Mutex<u64>)` — a generation counter; bumping it makes any running loop exit (no JoinHandle bookkeeping).
  - `panel_follow(app: AppHandle, state: State<FollowState>, term_program: Option<String>)` — `Some(tp)` (re)starts following; `None` stops.
  - Pure decision fn (unit-tested): `follow_tick(...) -> Tick`.

- [ ] **Step 1: Write failing tests for the pure decision logic**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn r(x: i32, y: i32) -> Rect { Rect { x, y, w: 800, h: 600 } }

    #[test]
    fn reposition_when_terminal_moved() {
        // terminal moved, panel untouched since our last set → follow
        let t = follow_tick(Some(r(0, 0)), Some(r(50, 0)), (900, 0), Some((900, 0)));
        assert!(matches!(t, Tick::Reposition(_)));
    }

    #[test]
    fn idle_when_nothing_changed() {
        let t = follow_tick(Some(r(0, 0)), Some(r(0, 0)), (900, 0), Some((900, 0)));
        assert!(matches!(t, Tick::Idle));
    }

    #[test]
    fn pause_when_user_dragged_panel() {
        // panel's actual pos differs from what we last set → user moved it
        let t = follow_tick(Some(r(0, 0)), Some(r(50, 0)), (300, 300), Some((900, 0)));
        assert!(matches!(t, Tick::Pause));
    }

    #[test]
    fn terminal_vanished_is_idle_not_fallback() {
        // don't teleport the panel to the screen edge mid-session (window
        // minimized / Space switch) — just wait for bounds to come back
        let t = follow_tick(Some(r(0, 0)), None, (900, 0), Some((900, 0)));
        assert!(matches!(t, Tick::Idle));
    }

    #[test]
    fn first_tick_with_no_expected_pos_repositions() {
        let t = follow_tick(None, Some(r(0, 0)), (0, 0), None);
        assert!(matches!(t, Tick::Reposition(_)));
    }
}
```

- [ ] **Step 2: Run, FAIL** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml follow`

- [ ] **Step 3: Implement**

```rust
//! P5 live-follow: permission-free polling of the terminal window's bounds.
//! Decision (2026-07-06): polling over AXObserver — zero permissions, ~600ms
//! worst-case lag is imperceptible for a companion panel.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, State};

use crate::geometry::Rect;
use crate::panel::pos::{reposition, terminal_window_bounds};

pub const FOLLOW_INTERVAL_MS: u64 = 600;

/// Generation counter: each (re)start bumps it; a loop exits when the
/// generation it was born with is no longer current.
#[derive(Default)]
pub struct FollowState(pub Mutex<u64>);

pub enum Tick {
    Reposition(Rect),
    Pause,
    Idle,
}

/// Pure per-tick decision. `expected_panel` is the position we set last time
/// (None on the first tick); `actual_panel` is where the window really is.
pub fn follow_tick(
    last_term: Option<Rect>,
    term: Option<Rect>,
    actual_panel: (i32, i32),
    expected_panel: Option<(i32, i32)>,
) -> Tick {
    if let Some(exp) = expected_panel
        && exp != actual_panel
    {
        return Tick::Pause; // user dragged the panel — stop fighting them
    }
    match (last_term, term) {
        (_, None) => Tick::Idle,
        (Some(a), Some(b)) if a == b => Tick::Idle,
        (_, Some(b)) => Tick::Reposition(b),
    }
}

#[tauri::command]
pub fn panel_follow(app: AppHandle, state: State<FollowState>, term_program: Option<String>) {
    let generation = {
        let mut g = state.0.lock().unwrap_or_else(|e| e.into_inner());
        *g += 1;
        *g
    };
    let Some(tp) = term_program else { return }; // None = stop (generation bump killed the old loop)
    std::thread::spawn(move || {
        let mut last_term: Option<Rect> = None;
        let mut expected: Option<(i32, i32)> = None;
        loop {
            std::thread::sleep(Duration::from_millis(FOLLOW_INTERVAL_MS));
            let st: State<FollowState> = app.state();
            if *st.0.lock().unwrap_or_else(|e| e.into_inner()) != generation {
                return; // superseded or stopped
            }
            let Some(win) = app.get_webview_window(super::PANEL_LABEL) else { return };
            let actual = match win.outer_position() {
                Ok(p) => (p.x, p.y),
                Err(_) => return,
            };
            let term = terminal_window_bounds(&win, &tp);
            match follow_tick(last_term, term, actual, expected) {
                Tick::Pause => return, // re-pin restarts via a fresh panel_follow call
                Tick::Idle => {}
                Tick::Reposition(t) => {
                    reposition(&win, Some(t));
                    expected = win.outer_position().ok().map(|p| (p.x, p.y));
                }
            }
            last_term = term;
        }
    });
}
```

Binding notes: `Rect` needs `PartialEq` + `Copy` — add derives in `geometry.rs` if missing. `super::PANEL_LABEL`: P1's `open_panel_window` creates the window with a label string — extract it into `pub const PANEL_LABEL: &str = "..."` in `panel/mod.rs` if it's currently inline, and use it in both places.

- [ ] **Step 4: Run, PASS** — `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml follow` (5 tests), then full `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/panel/ mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/src-tauri/capabilities/panel.json mur-hub-gui/src-tauri/src/geometry.rs
git commit -m "feat(hub): panel live-follow via CGWindowList polling (no permissions)"
```

---

### Task 2: Frontend — pin toggle + lifecycle

**Files:**
- Modify: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx` (header pin button + follow lifecycle)
- Modify: `mur-hub-gui/ui/src/components/panel/panel.css` (pin button active/paused styles)

**Interfaces:**
- Consumes: `panel_follow` (Task 1); `sess.terminal_program` (already in `PanelSession`).

- [ ] **Step 1: Implement.** State `const [pinned, setPinned] = useState(true);`. Effect:

```tsx
useEffect(() => {
  const tp = pinned ? sess?.terminal_program ?? null : null;
  invoke("panel_follow", { termProgram: tp }).catch(() => {});
  return () => { invoke("panel_follow", { termProgram: null }).catch(() => {}); };
}, [sess?.pid, sess?.terminal_program, pinned]);
```

Header (next to the session dropdown): a pin button `📌` with `className={pinned ? "panel-pin active" : "panel-pin"}`, `title="Follow terminal window"`, `onClick={() => setPinned(p => !p)}`. Note: a backend `Tick::Pause` exit leaves the loop dead while `pinned` is still true — clicking the pin off/on (or switching sessions) restarts it; acceptable for P5 (documented in the button's tooltip: "re-pin after moving the panel").

- [ ] **Step 2: Build** — `cd mur-hub-gui/ui && npm run build`. Expected: success.

- [ ] **Step 3: Manual verify** — Hub `.app` + `murmur` in iTerm2: open `/panel`, drag the **terminal** → panel trails it within ~1 s, through resize too; drag the **panel** away → it stays put (follow paused); toggle the pin off/on → it snaps back and follows again; quit murmur → no crash, loop exits.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/
git commit -m "feat(hub-ui): panel follow pin toggle"
```

---

### Task 3: Green + docs

- [ ] **Step 1:** `cargo fmt --all` (+ excluded Tauri crates via `--manifest-path`), clippy on the hub manifest, `npm run build`.
- [ ] **Step 2:** Update the parent spec's "Future" line + `docs/architecture/runtime-overview.md` panel section: live-follow shipped as polling (decision note), AXObserver explicitly retired unless latency complaints surface.
- [ ] **Step 3: Commit** — `git add -A && git commit -m "docs(panel): P5 live-follow shipped (polling)"`

---

## Self-Review

**Spec coverage:** parent spec reserved `reposition(target: Option<WindowBounds>)` as the stable seam — P5 uses exactly that seam, unchanged ✓. Edge cases the spec flagged for the follow feature: multiple windows (CGWindowList frontmost-of-owner, same behavior as snap-once ✓), Spaces/minimize (bounds vanish → `Tick::Idle`, no teleport ✓), user drag (pause ✓ — an addition the spec didn't enumerate but the "never fight the user" principle requires).

**Placeholders:** none; the two binding notes (Rect derives, PANEL_LABEL extraction) are concrete actions, not deferrals.

**Type consistency:** `follow_tick` signature matches all five tests; `panel_follow(term_program: Option<String>)` matches both frontend call sites (`{termProgram: tp}` / `{termProgram: null}` — Tauri camelCases `term_program`).

**Simplicity audit (ponytail):** no JoinHandle tracking (generation counter), no AX bindings, no config surface (a UI toggle only), poll loop is ~40 lines riding two existing functions.
