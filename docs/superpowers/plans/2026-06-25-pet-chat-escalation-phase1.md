# Pet Chat Escalation — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the two reported desktop-pet bugs (cramped CJK file-summary bubble; double-jump of Hub + chat on click) and build the glance→panel→full escalation skeleton.

**Architecture:** The pet OS window grows so the bubble is no longer clipped and gets CJK-correct typography; the existing per-agent `chat-{agent}` window becomes the "compact panel" — positioned next to the pet via a pure geometry function, raised above the always-on-top pet, and shown without stealing focus. Single-click opens it; a header button expands it to full size. Spec: `docs/superpowers/specs/2026-06-25-mur-pet-chat-escalation-redesign.md`.

**Tech Stack:** Rust + Tauri 2 (`mur-hub-gui/src-tauri`), React 18 + TypeScript + Vite (`mur-hub-gui/ui`). `mur-hub-gui` is workspace-EXCLUDED — build/test it via its own manifest.

## Global Constraints

- Brand name in any user-visible string is uppercase **MUR**; internal `name`/labels stay lowercase. (Not expected to appear in this phase.)
- No hardcoded magic values without a named const.
- `mur-hub-gui` is workspace-excluded: every cargo command uses `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`.
- The Tauri binary's `generate_context!` needs `mur-hub-gui/ui/dist/index.html` to exist or cargo compile fails. Before any cargo command, ensure it exists (a stub is fine for backend-only checks; do NOT commit the stub — it isn't gitignored). Per `mem:gotcha_hub_clippy_needs_ui_dist`.
- Frontend type/build check: `cd mur-hub-gui/ui && npm run build`.
- Coordinate space: all window-placement math is in **physical pixels** (monitor `.position()`/`.size()` are physical). Set positions with `tauri::PhysicalPosition`, never the builder's logical `.position(f64,f64)`, to stay consistent on Retina + external displays.
- Reply language for prose to the user is zh-TW; code/comments/commits in English (`mem:feedback_language_chinese`).

---

### Task 1: Pure window-placement geometry (`geometry.rs`)

A standalone module with **no Tauri types** so it unit-tests without the GUI stack. This is the only non-trivial logic in the phase and gets real TDD.

**Files:**
- Create: `mur-hub-gui/src-tauri/src/geometry.rs`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (add `mod geometry;` near the other `mod` declarations, ~top of file)
- Test: inline `#[cfg(test)]` in `geometry.rs`

**Interfaces:**
- Produces:
  - `pub struct Rect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }` with `pub fn right(&self)->i32`, `pub fn bottom(&self)->i32`
  - `pub fn anchor_panel(pet: Rect, panel: (i32,i32), mon: Rect) -> (i32,i32)`
  - `pub fn clamp_into(pos: (i32,i32), size: (i32,i32), mon: Rect) -> (i32,i32)`

- [ ] **Step 1: Write the failing tests**

Create `mur-hub-gui/src-tauri/src/geometry.rs`:

```rust
//! Pure window-placement geometry for the desktop pet and its chat panel.
//! Deliberately free of Tauri types so it unit-tests without the GUI stack.

/// A screen rectangle in PHYSICAL pixels, origin top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
}

/// Horizontal gap between the pet and its panel, physical px.
const PANEL_GAP: i32 = 8;

/// Place a `panel`-sized window adjacent to `pet`, preferring the LEFT side.
/// Flips to the right of the pet if the left placement would start before the
/// monitor's left edge. The result is always clamped fully inside `mon`.
pub fn anchor_panel(pet: Rect, panel: (i32, i32), mon: Rect) -> (i32, i32) {
    let (pw, ph) = panel;
    let left_x = pet.x - PANEL_GAP - pw;
    let right_x = pet.right() + PANEL_GAP;
    let x = if left_x >= mon.x { left_x } else { right_x };
    // Align the panel's top with the pet's top, then clamp.
    clamp_into((x, pet.y), (pw, ph), mon)
}

/// Clamp a window of `size` at `pos` so it stays fully inside `mon`.
/// If the window is larger than the monitor, it pins to the monitor origin.
pub fn clamp_into(pos: (i32, i32), size: (i32, i32), mon: Rect) -> (i32, i32) {
    let (w, h) = size;
    let max_x = (mon.right() - w).max(mon.x);
    let max_y = (mon.bottom() - h).max(mon.y);
    (pos.0.clamp(mon.x, max_x), pos.1.clamp(mon.y, max_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect { x: 0, y: 0, w: 1440, h: 900 };

    #[test]
    fn anchors_to_left_of_pet_when_room() {
        // pet at x=800; panel 380 wide fits to the left.
        let pet = Rect { x: 800, y: 100, w: 300, h: 260 };
        let (x, y) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(x, 800 - 8 - 380); // 412
        assert_eq!(y, 100);
    }

    #[test]
    fn flips_right_when_pet_near_left_edge() {
        // pet hugging the left edge: no room on the left, open to the right.
        let pet = Rect { x: 10, y: 100, w: 300, h: 260 };
        let (x, _) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(x, 10 + 300 + 8); // 318
    }

    #[test]
    fn clamps_y_so_panel_bottom_stays_on_screen() {
        // pet low on screen: panel top would push the 520-tall panel off bottom.
        let pet = Rect { x: 800, y: 700, w: 300, h: 260 };
        let (_, y) = anchor_panel(pet, (380, 520), MON);
        assert_eq!(y, 900 - 520); // 380
    }

    #[test]
    fn clamp_pulls_offscreen_window_in() {
        // bottom-right overflow.
        assert_eq!(clamp_into((1400, 880), (300, 260), MON), (1140, 640));
        // negative origin.
        assert_eq!(clamp_into((-50, -30), (300, 260), MON), (0, 0));
        // already in-bounds is unchanged.
        assert_eq!(clamp_into((100, 100), (300, 260), MON), (100, 100));
    }

    #[test]
    fn window_larger_than_monitor_pins_to_origin() {
        let tiny = Rect { x: 0, y: 0, w: 200, h: 150 };
        assert_eq!(clamp_into((50, 50), (300, 260), tiny), (0, 0));
    }
}
```

- [ ] **Step 2: Add the module declaration**

In `mur-hub-gui/src-tauri/src/lib.rs`, add alongside the existing top-level `mod` lines (e.g. next to `mod chat_window;` / `mod pet;`):

```rust
mod geometry;
```

- [ ] **Step 3: Run tests to verify they pass**

Ensure the dist stub exists first, then test only this module:

```bash
[ -f mur-hub-gui/ui/dist/index.html ] || { mkdir -p mur-hub-gui/ui/dist && echo '<!doctype html><title>stub</title>' > mur-hub-gui/ui/dist/index.html; }
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml geometry:: -- --nocapture
```

Expected: 5 tests pass (`anchors_to_left_of_pet_when_room`, `flips_right_when_pet_near_left_edge`, `clamps_y_so_panel_bottom_stays_on_screen`, `clamp_pulls_offscreen_window_in`, `window_larger_than_monitor_pins_to_origin`).

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/geometry.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(pet): pure window-placement geometry (anchor + clamp)"
```

---

### Task 2: Monitor-rect helper + clamp the pet on spawn

Use Task 1's `clamp_into` so a corner drop can't push the (now larger) pet off-screen.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs:165-183` (the `pos` block + `WebviewWindowBuilder`)

**Interfaces:**
- Consumes: `geometry::{Rect, clamp_into}` from Task 1.
- Produces: `fn monitor_rect_for_point(app: &AppHandle, x: i32, y: i32) -> geometry::Rect` (file-private in `pet/mod.rs`), reused by Task 3.

- [ ] **Step 1: Add the monitor helper**

In `mur-hub-gui/src-tauri/src/pet/mod.rs`, add near the other file-private helpers (after `mur_home`, ~line 42). Uses `available_monitors` (physical coords) with a primary fallback, so it doesn't depend on `monitor_from_point` availability:

```rust
use crate::geometry;

/// Physical-pixel rect of the monitor containing `(x, y)`, falling back to the
/// primary monitor, then to a 1440x900 origin rect if no monitor is reported.
fn monitor_rect_for_point(app: &AppHandle, x: i32, y: i32) -> geometry::Rect {
    let to_rect = |m: &tauri::Monitor| {
        let p = m.position();
        let s = m.size();
        geometry::Rect { x: p.x, y: p.y, w: s.width as i32, h: s.height as i32 }
    };
    if let Ok(mons) = app.available_monitors() {
        if let Some(m) = mons.iter().find(|m| {
            let r = to_rect(m);
            x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
        }) {
            return to_rect(m);
        }
    }
    if let Ok(Some(m)) = app.primary_monitor() {
        return to_rect(&m);
    }
    geometry::Rect { x: 0, y: 0, w: 1440, h: 900 }
}
```

> `// ponytail: available_monitors + contains-check instead of monitor_from_point — fewer API assumptions, same result.`

- [ ] **Step 2: Clamp the spawn position and set it physically**

Replace the `pos`/builder position handling at `pet/mod.rs:165-183`. New pet size is `300x260` (Task 6 also changes `inner_size`). The clamp must use the SAME size as `inner_size`. Define a const and reuse:

```rust
const PET_W: i32 = 300;
const PET_H: i32 = 260;
```
(place near the top-of-file consts, e.g. above `PET_DROP_MAX_FILES`)

Then, in `pet_spawn_at`, replace the `pos` struct + builder `.inner_size(200.0,200.0).position(pos.x, pos.y)` usage with a clamped physical position:

```rust
let mon = monitor_rect_for_point(&app, screen_x as i32, screen_y as i32);
let (cx, cy) = geometry::clamp_into((screen_x as i32, screen_y as i32), (PET_W, PET_H), mon);

let url_path = format!("index.html#/pet/{}", urlenc(&agent_name));

let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url_path.into()))
    .transparent(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible_on_all_workspaces(true)
    .shadow(false)
    .inner_size(PET_W as f64, PET_H as f64)
    .visible(false) // Task 6: avoid the opaque-square entrance flash
    .build()
    .map_err(|e| e.to_string())?;
let _ = win.set_position(tauri::PhysicalPosition::new(cx, cy));
let _ = win.show();
```

(Delete the now-unused `PetPosition { ... }` `pos` binding here; the struct itself is removed in a later phase, leave it defined for now to avoid touching `pet_reposition`.)

- [ ] **Step 3: Build to verify it compiles**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --bin mur-hub
```
(If the bin name differs, use `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml` to build all targets.)
Expected: compiles clean (warnings about unused `PetPosition` field are acceptable this phase).

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/pet/mod.rs
git commit -m "feat(pet): clamp pet spawn into its monitor; physical positioning"
```

---

### Task 3: Anchor the chat panel to the pet + raise it + don't steal focus

Make `open_chat_window` open at a compact size positioned beside the pet, above the always-on-top pet, without stealing focus.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/chat_window.rs:27-64`

**Interfaces:**
- Consumes: `geometry::anchor_panel`, `pet`-window label scheme (`pet-{safe}`), the existing `label()` (`chat-{safe}`).
- Produces: unchanged public signature `open_chat_window(agent_name: String, app: AppHandle) -> Result<(), String>` (the JS relay caller is untouched).

- [ ] **Step 1: Add panel size consts + a pet-label helper**

At the top of `chat_window.rs`:

```rust
/// Compact ("panel") default size; the user can expand to full via the header.
const PANEL_W: i32 = 380;
const PANEL_H: i32 = 520;

fn pet_label(agent_name: &str) -> String {
    // Same safe-name transform as `label`, with the pet prefix.
    let safe: String = agent_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    format!("pet-{}", safe)
}
```

- [ ] **Step 2: Rework `open_chat_window` body**

Replace lines 27-64 with: compact size, no `set_focus` (so it never pulls focus / raises the wrong window), `always_on_top` so it sits above the pet, and anchored position computed from the pet window. The existing-window guard also drops `set_focus`.

```rust
#[tauri::command]
pub fn open_chat_window(agent_name: String, app: AppHandle) -> Result<(), String> {
    let lbl = label(&agent_name);

    // Single-instance guard: just show (do NOT set_focus — that steals focus
    // from the user's foreground app and previously raised the Hub too).
    if let Some(win) = app.get_webview_window(&lbl) {
        let _ = win.show();
        let _ = win.set_focus(); // existing window: user explicitly re-opened it
        return Ok(());
    }

    let url = format!("index.html#/chat/{}", urlenc(&agent_name));

    let win = WebviewWindowBuilder::new(&app, &lbl, WebviewUrl::App(url.into()))
        .title(&agent_name)
        .inner_size(PANEL_W as f64, PANEL_H as f64)
        .min_inner_size(320.0, 420.0)
        .resizable(true)
        .decorations(false)
        .transparent(true)
        .shadow(true)
        .always_on_top(true) // sit above the always-on-top pet
        .visible(false)
        .build()
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "macos")]
    let _ = win.set_effects(
        EffectsBuilder::new()
            .effect(Effect::HudWindow)
            .state(EffectState::Active)
            .radius(12.0)
            .build(),
    );

    // Anchor next to the pet (if it exists) on the pet's monitor.
    if let Some(pet) = app.get_webview_window(&pet_label(&agent_name)) {
        if let (Ok(pp), Ok(ps)) = (pet.outer_position(), pet.outer_size()) {
            let pet_rect = crate::geometry::Rect {
                x: pp.x, y: pp.y, w: ps.width as i32, h: ps.height as i32,
            };
            let mon = crate::pet::monitor_rect_for_point(&app, pp.x, pp.y);
            let (x, y) = crate::geometry::anchor_panel(pet_rect, (PANEL_W, PANEL_H), mon);
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }

    win.show().map_err(|e| e.to_string())?;
    Ok(())
}
```

> Note: `monitor_rect_for_point` from Task 2 must be `pub(crate)` for this cross-module call. Change its `fn` to `pub(crate) fn` in `pet/mod.rs`.

- [ ] **Step 3: Make the helper crate-visible**

In `pet/mod.rs`, change `fn monitor_rect_for_point` → `pub(crate) fn monitor_rect_for_point`.

- [ ] **Step 4: Build to verify it compiles**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
Expected: compiles clean.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/chat_window.rs mur-hub-gui/src-tauri/src/pet/mod.rs
git commit -m "feat(pet): anchor chat panel to pet, raise above it, no focus steal"
```

---

### Task 4: Stop the double-jump (remove dashboard show/focus)

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs:271-280` (`pet_open_chat`)

**Interfaces:**
- Consumes: nothing new. The hidden dashboard's `pet-open-chat` listener (`DashboardApp.tsx:506`) still relays to `open_chat_window` and stages the file-drop draft — verified the dashboard is hidden-not-destroyed on close (`lib.rs:394-396`).

- [ ] **Step 1: Drop the dashboard raise**

Replace `pet_open_chat` (271-280) with:

```rust
/// Open `agent_name`'s chat panel. The (hidden) dashboard webview relays the
/// `pet-open-chat` event to `open_chat_window` and stages any `draft`; we must
/// NOT show/focus the dashboard here — that caused the Hub to "jump" alongside
/// the chat window.
#[tauri::command]
pub fn pet_open_chat(agent_name: String, draft: Option<String>, app: AppHandle) {
    let _ = app.emit(
        "pet-open-chat",
        serde_json::json!({ "agent": agent_name, "draft": draft }),
    );
}
```

- [ ] **Step 2: Build**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/src-tauri/src/pet/mod.rs
git commit -m "fix(pet): don't raise the Hub when opening chat (double-jump)"
```

---

### Task 5: Enlarge the pet root + click-through transparent areas (CSS)

The React side must match the new 300×260 OS window (Task 2) and the bubble must no longer be the click-catcher across the whole window.

**Files:**
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css:7-14` (`.pet-root`), `:15-25` (`.pet-sprite` margin)

**Interfaces:** none (pure CSS).

- [ ] **Step 1: Resize the root and pass clicks through empty space**

Replace `.pet-root` (lines 7-14):

```css
.pet-root {
  width: 300px;
  height: 260px;
  background: transparent;
  overflow: visible;
  user-select: none;
  -webkit-user-select: none;
  /* Empty transparent area must not eat clicks meant for apps behind the pet. */
  pointer-events: none;
}
/* Re-enable pointer events only on the actual interactive children. */
.pet-sprite,
.pet-bubble,
.pet-context-menu { pointer-events: auto; }
```

Anchor the sprite to the bottom so the bubble has room above it. Change `.pet-sprite` `margin` (line 18) from `margin: 20px auto 0;` to:

```css
  margin: auto auto 0;  /* sprite sits at the bottom-center of the 260px window */
```

- [ ] **Step 2: Verify the frontend builds**

```bash
cd mur-hub-gui/ui && npm run build
```
Expected: build succeeds (no TS/CSS errors).

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/styles/components/pet.css
git commit -m "feat(pet): enlarge pet root to 300x260, click-through empty area"
```

---

### Task 6: CJK-correct bubble typography + faux-glass + html lang

Fix the actual reported rendering: readable multi-character CJK lines.

**Files:**
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (`.pet-bubble` block ~151-172, `.pet-bubble-text` 188-192, tail `::after` 178-187)
- Modify: `mur-hub-gui/ui/src/i18n/index.tsx:40-42` (the lang effect)

**Interfaces:** none.

- [ ] **Step 1: Sync `<html lang>` to the active language**

In `mur-hub-gui/ui/src/i18n/index.tsx`, extend the existing effect (currently only writes localStorage):

```tsx
  useEffect(() => {
    localStorage.setItem(STORAGE_KEY, lang);
    document.documentElement.lang = lang; // WebKit uses this to pick the
    // correct Han glyph variant (TC vs SC/JP) and CJK line-breaking rules.
  }, [lang]);
```

- [ ] **Step 2: CJK-safe bubble text + roomier bubble + faux-glass**

In `pet.css`, update `.pet-bubble-text` (188-192):

```css
.pet-bubble-text {
  font-size: var(--text-sm);
  color: var(--text-primary);
  line-height: 1.7;            /* CJK needs more leading */
  text-align: left;           /* never justify CJK */
  overflow-wrap: anywhere;    /* replaces non-standard word-break: break-word */
}
```

In the `.pet-bubble` rule (~151-172), widen the readable column to use the bigger window and make it frosted glass (consistent with the chat window's HudWindow). Set `max-width: 260px;` (fits the 300px window with padding) and ensure the background is translucent + blurred:

```css
  max-width: 260px;
  min-width: 140px;
  background: color-mix(in srgb, var(--surface-card) 78%, transparent);
  -webkit-backdrop-filter: blur(18px) saturate(140%);
  backdrop-filter: blur(18px) saturate(140%);
  border: 1px solid color-mix(in srgb, var(--border-line) 60%, transparent);
```
(Keep the existing `position`, `padding`, `border-radius`, `box-shadow`, `z-index`, animation properties in that rule — only widen + reskin the fill.)

Update the tail `::after` (line 185) so it matches the translucent fill instead of the opaque `--surface-card`:

```css
  border-top-color: color-mix(in srgb, var(--surface-card) 78%, transparent);
```

- [ ] **Step 3: Verify build**

```bash
cd mur-hub-gui/ui && npm run build
```
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/styles/components/pet.css mur-hub-gui/ui/src/i18n/index.tsx
git commit -m "fix(pet): CJK-correct bubble typography, faux-glass, html lang sync"
```

---

### Task 7: Single-click opens chat; expand button on the panel

Make single-click the primary action (research: double-click is a relic) and give the panel a header control to grow to full size.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx:166-171` (`handleMouseUp`)
- Modify: `mur-hub-gui/ui/src/components/chat/AgentChatWindow.tsx` (header area, ~88-103)

**Interfaces:**
- Consumes: existing `handleChat` (`PetApp.tsx:188-191`).

- [ ] **Step 1: Single-click opens chat (and still greets)**

In `PetApp.tsx`, update `handleMouseUp` (166-171) so a real click both fires the greeting event and opens the panel. Double-click still calls `handleChat` (idempotent via the single-instance guard), so no single/double disambiguation timer is needed:

```tsx
  function handleMouseUp() {
    if (pressRef.current && Date.now() - clickTimeRef.current < CLICK_MS) {
      // Greeting expression + open the chat panel (single-click is primary).
      invoke("hub_emit_event", { agentName, eventName: "user.click.pet" }).catch(() => {});
      void handleChat();
    }
    pressRef.current = null;
  }
```

Update the sprite's `title` to reflect single-click (optional copy tweak) — leave `onDoubleClick={handleChat}` in place as a redundant shortcut.

- [ ] **Step 2: Add an expand-to-full button in the chat window header**

In `AgentChatWindow.tsx`, add a small header control that resizes this window to the full 780×660. Import at top:

```tsx
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
```

Add a handler in the component body:

```tsx
  const expandToFull = () =>
    void getCurrentWindow().setSize(new LogicalSize(780, 660)).catch(() => {});
```

Render an expand button in the existing header/toolbar region (near where `displayName`/`status` are shown, ~lines 88-103). Match the surrounding markup; e.g.:

```tsx
  <button
    className="chat__expand"
    onClick={expandToFull}
    title={t("chat.expand")}
    aria-label={t("chat.expand")}
  >⤢</button>
```

Add the `chat.expand` i18n key (e.g. en: `"Expand"`, zh-TW: `"放大"`) in the translation tables the other `chat.*` keys live in. (Locate via `grep -rn '"chat.stop"' mur-hub-gui/ui/src/i18n`.)

- [ ] **Step 3: Verify build (type-checks the i18n key + imports)**

```bash
cd mur-hub-gui/ui && npm run build
```
Expected: build succeeds. A failure about a missing `chat.expand` key means Step 2's i18n entry was not added to every locale table — add it.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/components/chat/AgentChatWindow.tsx mur-hub-gui/ui/src/i18n
git commit -m "feat(pet): single-click opens chat panel; expand-to-full button"
```

---

### Task 8: Build the full app and verify behavior (manual E2E)

Window-server behavior (focus, z-order, anchoring, CJK rendering) can't be unit-tested; this task is the gate.

**Files:** none (build + observe).

- [ ] **Step 1: Build the real UI + app bundle**

```bash
cd mur-hub-gui/ui && npm run build && cd -
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml --release
```
(For a runnable `.app` to drive with Computer Use, use `cargo tauri build --debug --bundles app` per `mem:gotcha_tauri_dev_null_bundleid_computeruse`; `cargo tauri dev` is invisible to screenshots.)

- [ ] **Step 2: Verify each behavior**

Spawn a pet (drag an agent out of the Hub) and confirm:

- [ ] Pet spawns with **no opaque-square flash**.
- [ ] **Single-click** the pet → chat panel opens (~380×520) **next to the pet**, the **Hub does NOT appear/jump**, and **focus stays** in whatever app was frontmost.
- [ ] The panel sits **above** the pet (pet does not paint over it).
- [ ] Drag the pet to a **second monitor** / screen corner → panel still opens on the **pet's** monitor, fully on-screen; pet itself never clips off-screen.
- [ ] The **expand ⤢** button grows the panel to 780×660.
- [ ] Drop a **Chinese text file** on the pet → the take bubble shows **readable multi-character lines** (not 3-4 glyphs/line). (Summary-in-panel is Phase 3; readable bubble is the Phase 1 bar.)

- [ ] **Step 3: Lint (clippy + fmt for the excluded crate)**

```bash
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- -D warnings
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
(Per `mem:gotcha_ci_fmt_excluded_crates`, the excluded crate needs its own fmt invocation.)
Expected: clippy clean, fmt makes no changes (or commit the formatting).

- [ ] **Step 4: Commit any fmt/clippy fixups**

```bash
git add -A && git commit -m "chore(pet): clippy/fmt for chat escalation phase 1" || true
```

---

## Self-Review

**Spec coverage (Phase 1 slice of `2026-06-25-mur-pet-chat-escalation-redesign.md`):**
- §4 panel anchoring → Task 1 (geometry) + Task 3 (apply). ✓
- §4 z-order (panel above pet) → Task 3 (`always_on_top`). ✓
- §4 off-screen spawn clamp → Task 1 + Task 2. ✓
- §4 pet window enlarge → Task 2 (OS size) + Task 5 (CSS). ✓
- §5 double-jump / no focus steal → Task 3 (`open_chat_window` no set_focus) + Task 4 (`pet_open_chat`). ✓
- §7 CJK typography + `html lang` → Task 6. ✓
- §8 single-click primary → Task 7. ✓
- §9 entrance flash → Task 2 (`visible(false)`+`show()`). ✓
- §9 faux-glass bubble → Task 6. ✓
- Deferred to later plans (intentional): file-summary→panel deep integration (§6), keyboard a11y / mute persistence / dead-code deletion (§8/§9 Phase 2), drag-over affordance / privacy disclosure / drop honesty / mascot motion (§6/§9 Phase 3). Noted, not dropped.

**Placeholder scan:** No TBD/TODO; every code step shows real code; the one lookup ("locate the `chat.*` i18n table via grep") names the exact command and the failure signal that catches a miss.

**Type consistency:** `geometry::Rect`/`anchor_panel`/`clamp_into` signatures match across Tasks 1→2→3; `monitor_rect_for_point` is defined in Task 2 and made `pub(crate)` before its cross-module use in Task 3; `PET_W/PET_H` (Task 2) and `PANEL_W/PANEL_H` (Task 3) are the single source for the sizes used in both OS-window and clamp math.

**Coordinate space:** all placement uses physical px + `PhysicalPosition`; monitor rects come from `available_monitors()`/`primary_monitor()` (physical). Flagged in Global Constraints and verified on a second monitor in Task 8.
