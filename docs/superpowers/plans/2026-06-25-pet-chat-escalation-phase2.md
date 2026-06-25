# Pet Chat Escalation — Phase 2 Implementation Plan (cheap wins + hardening)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the low-risk, high-value desktop-pet improvements deferred from Phase 1: keyboard/accessibility, mute persistence + indicator, contrast, and deletion of the write-only pet-position dead code.

**Architecture:** Small, isolated edits to the existing pet UI (`PetApp.tsx`, `PetFace.tsx`, `pet.css`) and backend (`pet/mod.rs`, `lib.rs`). No new windows, no new dependencies. Builds on Phase 1 (branch `feat/pet-chat-escalation`); base = Phase 1 head.

**Tech Stack:** Rust + Tauri 2 (`mur-hub-gui/src-tauri`), React 18 + TypeScript + Vite (`mur-hub-gui/ui`).

## Global Constraints

- `cargo` is NOT on PATH: prepend `export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`.
- `mur-hub-gui` is workspace-EXCLUDED: every cargo command uses `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`; the bin is `mur-hub-gui`. Frontend check: `cd mur-hub-gui/ui && npm run build`.
- Stage ONLY the files each task names — NEVER `git add -A`. The working tree may carry unrelated user changes (`.mcp.json`, `Cargo.lock`); leave them alone.
- Never delete files or run disk cleanup; if a build fails on disk, stop and report.
- Brand "MUR" uppercase in user-visible strings; internal `name`/labels lowercase.
- Line numbers below are indicative (Phase 1 shifted them) — grep/read the current file to locate the exact spot before editing.

---

### Task 1: Keyboard-operable pet sprite + a11y

Make the pet operable without a mouse (today the sprite is a plain `<div>` — no focus, no key handler), and fix the lowest-contrast control.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (the `.pet-sprite` element; the Escape `keydown` effect; the context-menu render)
- Modify: `mur-hub-gui/ui/src/components/PetFace.tsx` (`role="img"` aria-label)
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (`.pet-bubble-close` color; focus-visible style)

- [ ] **Step 1: Make the sprite a real button**

Find the `<div className={`pet-sprite ...`} ...>` element. Add keyboard semantics (keep the existing mouse handlers + `onDoubleClick`):

```tsx
      <div
        className={`pet-sprite pet-sprite--${expression}`}
        role="button"
        tabIndex={0}
        aria-label={t("pet.chat")}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onDoubleClick={handleChat}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            void handleChat();
          }
        }}
      >
```
Remove the now-redundant `title={t("pet.chat")}` (the `aria-label` replaces it; keep it only if you want a hover tooltip).

- [ ] **Step 2: Hide the inner PetFace/img from the a11y tree**

The sprite button now carries the label; the inner graphic should not double-announce. On the `<PetFace .../>` usage and the `<img ... className="pet-image" .../>`, add `aria-hidden`:
- `<img src={imageSrc} alt="" aria-hidden className="pet-image" draggable={false} />` (empty alt + aria-hidden)
- For `<PetFace .../>`: in `PetFace.tsx`, change the root SVG `role="img" aria-label={...}` to `aria-hidden` (drop the unlocalized `${presetId} pet, ${expression}` label — the parent button is now the accessible name). Locate the `role="img"` line in PetFace.tsx and replace `role="img" aria-label={...}` with `aria-hidden`.

- [ ] **Step 3: Escape also closes the context menu; focus first item on open**

In the existing `keydown` effect that closes the bubble on Escape, also close the context menu:
```tsx
      if (e.key === "Escape") {
        if (contextMenu.visible) setContextMenu((m) => ({ ...m, visible: false }));
        if (bubble) setBubble(null);
      }
```
(Adjust the effect's dependency array to include `contextMenu.visible`.) When the menu opens, focus its first item: add a `ref` to the first `.pet-menu-item` button and a `useEffect` on `contextMenu.visible` that calls `firstItemRef.current?.focus()`. Skip arrow-key roving (Tab already moves between the `<button>`s).

- [ ] **Step 4: Bubble close-button contrast + focus-visible**

In `pet.css`, `.pet-bubble-close` resting `color: var(--text-tertiary)` likely fails WCAG 4.5:1 — change to `color: var(--text-secondary);`. Add a visible keyboard focus ring for the sprite + menu items:
```css
.pet-sprite:focus-visible,
.pet-menu-item:focus-visible,
.pet-bubble-close:focus-visible {
  outline: 2px solid var(--color-brand);
  outline-offset: 2px;
}
```

- [ ] **Step 5: Verify build**

```bash
cd mur-hub-gui/ui && npm run build
```
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/components/PetFace.tsx mur-hub-gui/ui/src/styles/components/pet.css
git commit -m "feat(pet): keyboard-operable sprite, a11y labels, Escape closes menu, contrast"
```

---

### Task 2: Mute persistence + visible indicator

Mute is React-state only today — every reload/respawn silently un-mutes, and the state is invisible.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (`muted` state init + `handleToggleMute`)
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (a `.pet-sprite--muted` badge)

- [ ] **Step 1: Persist mute to localStorage, keyed by agent**

Replace the `muted` state initializer to read persisted state (lazy initializer), and write on toggle. The key includes `agentName` (from `getAgentName()`):
```tsx
  const muteKey = `pet-muted:${agentName}`;
  const [muted, setMuted] = useState(() => localStorage.getItem(muteKey) === "1");
  const mutedRef = useRef(muted);
```
In `handleToggleMute`, persist:
```tsx
  function handleToggleMute() {
    closeMenu();
    setMuted((m) => {
      const next = !m;
      mutedRef.current = next;
      localStorage.setItem(muteKey, next ? "1" : "0");
      if (next) setBubble(null);
      return next;
    });
  }
```
(Also ensure `mutedRef.current` is seeded from the initial `muted` — set `const mutedRef = useRef(muted)` as above.)

- [ ] **Step 2: Show a 🔇 badge on the sprite when muted**

Add the muted class to the sprite container conditionally: `className={`pet-sprite pet-sprite--${expression}${muted ? " pet-sprite--muted" : ""}`}`. In `pet.css`:
```css
.pet-sprite--muted::after {
  content: "🔇";
  position: absolute;
  right: 2px;
  bottom: 2px;
  font-size: 16px;
  filter: drop-shadow(0 1px 2px rgba(0,0,0,0.4));
}
```
(Ensure `.pet-sprite` is `position: relative` so the badge anchors to it; add `position: relative` to `.pet-sprite` if absent.)

- [ ] **Step 3: Verify build + commit**

```bash
cd mur-hub-gui/ui && npm run build
git add mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/styles/components/pet.css
git commit -m "feat(pet): persist mute per-agent + muted badge"
```

---

### Task 3: Delete the write-only pet-position dead code

`pet_position.json` is written on every move but never read; the "restore on restart" it implies was never built and is intentionally not wanted (spec §12). Remove the whole path.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs` (delete `PetPosition`, `pet_position_path`, `save_position`, `pet_reposition`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (remove `pet::pet_reposition` from `tauri::generate_handler!`)
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (remove the `tauri://move` → `pet_reposition` listener effect)

- [ ] **Step 1: Confirm nothing else reads it**

```bash
export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
grep -rn "PetPosition\|pet_reposition\|pet_position\|save_position\|display_id" mur-hub-gui/src-tauri/src mur-hub-gui/ui/src
```
Expected references: only the definitions + the `tauri://move` listener + the handler registration (all to be removed). If anything else reads them, stop and report.

- [ ] **Step 2: Delete the Rust dead code**

In `pet/mod.rs`, delete the `PetPosition` struct, `pet_position_path()`, `save_position()`, and the `#[tauri::command] pub fn pet_reposition(...)`. Confirm `pet_spawn_at` no longer references `PetPosition` (Phase 1 already removed the local `pos` binding; if a stray reference remains, remove it). Remove the now-unused `dirs`/`serde` imports only if they become unused (check — they're likely used elsewhere; do not remove shared imports).

- [ ] **Step 3: Remove the handler registration**

In `lib.rs` `tauri::generate_handler![ ... ]`, delete the `pet::pet_reposition,` line.

- [ ] **Step 4: Remove the frontend listener**

In `PetApp.tsx`, delete the `useEffect` that listens to `win.listen("tauri://move", ...)` and invokes `pet_reposition` (the position-persistence effect).

- [ ] **Step 5: Build (Rust + frontend) to verify nothing breaks**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cd mur-hub-gui/ui && npm run build && cd -
```
Expected: both compile clean (no "unused" errors from the deletion; if `pet_reposition` removal leaves a dangling reference, the build will catch it).

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/pet/mod.rs mur-hub-gui/src-tauri/src/lib.rs mur-hub-gui/ui/src/components/PetApp.tsx
git commit -m "refactor(pet): delete write-only pet_position persistence (dead code)"
```

---

### Task 4: Minor cleanups (stale comment + geometry edge test)

**Files:**
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (stale `.pet-bubble` comment)
- Modify: `mur-hub-gui/src-tauri/src/geometry.rs` (add a non-zero-monitor-origin test)

- [ ] **Step 1: Fix the stale bubble comment**

The `.pet-bubble` comment still says "fixed transparent 200×200" — update to reflect the 300×260 window (Phase 1). Find the comment above `.pet-bubble` and correct the dimensions.

- [ ] **Step 2: Add a secondary-display geometry test (TDD-style, real assertions)**

In `geometry.rs` `#[cfg(test)] mod tests`, add a test exercising a monitor with a non-zero origin (e.g. a second display at x=1920), so `mon.x/mon.y` are used as the lower clamp bound:
```rust
    #[test]
    fn anchors_on_a_secondary_monitor_origin() {
        let mon = Rect { x: 1920, y: 0, w: 1440, h: 900 };
        let pet = Rect { x: 2000, y: 100, w: 300, h: 260 };
        // room on the left within this monitor: 2000 - 8 - 380 = 1612 >= 1920? no -> flip right
        let (x, y) = anchor_panel(pet, (380, 520), mon);
        assert_eq!(x, 2000 + 300 + 8); // 2308, opens right
        assert_eq!(y, 100);
    }

    #[test]
    fn clamps_within_secondary_monitor_bounds() {
        let mon = Rect { x: 1920, y: 0, w: 1440, h: 900 };
        // a point left of this monitor's origin clamps up to mon.x
        assert_eq!(clamp_into((1900, 50), (300, 260), mon), (1920, 50));
    }
```

- [ ] **Step 3: Run the geometry tests**

```bash
[ -f mur-hub-gui/ui/dist/index.html ] || { mkdir -p mur-hub-gui/ui/dist && echo '<!doctype html><title>stub</title>' > mur-hub-gui/ui/dist/index.html; }
cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml geometry:: -- --nocapture
```
Expected: all geometry tests pass (now including the two secondary-monitor cases).

- [ ] **Step 4: Verify frontend build + commit**

```bash
cd mur-hub-gui/ui && npm run build && cd -
git add mur-hub-gui/ui/src/styles/components/pet.css mur-hub-gui/src-tauri/src/geometry.rs
git commit -m "chore(pet): fix stale bubble comment + secondary-monitor geometry tests"
```

---

### Task 5: Verify + lint

- [ ] **Step 1: clippy + fmt (excluded crate needs its own invocation)**

```bash
export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- --check
```
Expected: no clippy warnings in touched files; fmt clean. Fix any (collapse nested if-let into let-chains if clippy asks), then commit fmt fixups (`git add` the touched Rust files only).

- [ ] **Step 2: Manual a11y check**

Tab to the pet (focus ring visible), press Enter/Space → chat opens. Right-click → menu; press Escape → menu closes. Mute → 🔇 badge shows; reload the pet window → still muted.

## Self-Review

- §8 keyboard-operable sprite + Escape-closes-menu → Task 1. ✓
- §8 mute persistence + badge → Task 2. ✓
- §8 contrast (bubble close) → Task 1 Step 4. ✓
- §9 delete write-only pet_position dead code → Task 3. ✓
- Final-review minors: stale `.pet-bubble` comment (M2) → Task 4; non-zero-monitor geometry test (T1) → Task 4. ✓
- No placeholders; every code step shows real code; the one lookup (Task 3 Step 1 grep) names the command + the stop condition.
- Type consistency: `muteKey`/`mutedRef` consistent in Task 2; deletions in Task 3 are verified by the grep + the build catching dangling refs.
