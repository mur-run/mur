# MUR Desktop Pet — Chat & Interaction Redesign

**Date:** 2026-06-25
**Status:** Design — pending implementation plan
**Scope:** `mur-hub-gui` desktop pet (per-agent always-on-top macOS mascot, Tauri 2 + React/Vite)

## 1. Problem

Two reported bugs, plus a broader "make the pet chat the best it can be" ask backed by 2025-2026 desktop-companion UX research.

- **Bug 1 — cramped file-summary bubble.** Dropping a text file shows the agent's summary in a bubble trapped inside the 200×200 pet window (`pet.css:151-172`). After `max-width:188px` minus padding, the text column is ~110-120px; CJK (no spaces) breaks to 3-4 glyphs/line. It's a window-size problem, not a font problem — no CSS escapes a 200px window (`pet.css:2-6` documents that anything painted outside is clipped).
- **Bug 2 — double-jump.** Double-clicking the pet raises the **Hub dashboard AND** the chat window. `pet_open_chat` (`pet/mod.rs:270-280`) calls `dashboard.show()+set_focus()` only to use the dashboard as an event relay that then opens the chat window. The relay is unnecessary.

## 2. Goals / Non-goals

**Goals:** a coherent glance→panel→full escalation model; both bugs fixed at the root; CJK-correct and accessible by default; honest about where dropped data goes; one consistent material language.

**Non-goals (explicit YAGNI):** frontend-stack rewrite (React/Vite/Tauri stays — every issue is CSS + window logic); `tauri-nspanel` native shim (deferred, see §5); voice; RTL; pet restore-on-restart; first-run tutorial; live display-disconnect handling; fullscreen-Space float guarantees. Rationale per finding in §10.

## 3. The escalation model

Three states; the lazy insight is that the panel **is the existing chat window, resized** — no new chat surface to build.

1. **Glance bubble** — on the pet. Short ambient one-liners only (status, nudges). Never file summaries, never conversations.
2. **Compact chat panel** — the existing `chat-{agent}` window opened small (~380×520), anchored beside the pet. Full `AgentChatWindow`/`ChatTab` inside (history + composer + streaming) — zero new UI.
3. **Full** — same window, "expand" button → 780×660.

> `// ponytail: one window, two sizes — not a separate panel window + a separate full window.`

## 4. Window topology & positioning

- **Pet window** grows from 200×200 to **~300×260** so the bubble has room. Sprite stays 160px anchored bottom-center; transparent non-sprite areas pass clicks through (`pointer-events:none` on root, `auto` on sprite + bubble). `body.pet-window` already transparent.
- **Panel** = `chat-{agent}` window. New compact default size + the existing 780×660 reachable via expand.
- **Anchoring (REQUIRED — currently missing).** `open_chat_window` sets no position → Tauri centers on the **primary** monitor, so the panel never lands by the pet, and lands on the wrong screen in multi-display setups. Fix: before `show()`, read the pet window's `outer_position()` + `outer_size()` and its monitor, place the panel adjacent (default to the left of the pet; flip right if it would cross the monitor edge), clamp into the monitor work area. Pass the agent/pet label into `open_chat_window` so it can find the pet. (`chat_window.rs:27-64`)
- **Z-order (REQUIRED).** The pet is `always_on_top`; the chat window is not (`pet/mod.rs:176`). An anchored panel would be painted *under* the pet and lose clicks in the overlap. Fix: give the panel `always_on_top(true)` and raise it after the pet.
- **Off-screen clamp on spawn.** `pet_spawn_at` positions at the raw drop point with no bounds check (`pet/mod.rs:165-181`); a corner drop clips the pet, worse at 300×260. Clamp x/y into the work area of the monitor under the drop point (reuse the `primary_monitor` fallback pattern from `lib.rs:118`).

## 5. Focus / double-jump fix

`pet_open_chat` calls `open_chat_window` **directly (Rust→Rust)** — no dashboard `show()/set_focus()`, no event relay. The `draft` reaches the chat window via a window-targeted event after it opens. The panel is shown **without `set_focus()`** so it never pulls focus from the user's foreground app. This removes the double-jump at the root.

> `// ponytail: no tauri-nspanel dependency in v1. The focus-steal came from explicit dashboard.show()+set_focus(); removing those fixes the reported bug. Add a non-activating NSPanel only if testing shows residual focus-steal (keyboard-into-panel, cmd-tab exclusion).`

## 6. File-drop redesign

- **Summary leaves the bubble.** On drop: open the panel, post the dropped file(s) as a **chip**, and stream the agent's take as a real assistant message you can reply to. The bubble shows only a transient `📄 reading… → ✓ opened`. Bug 1 disappears — summaries live in the roomy panel. (`pet/mod.rs:424-461`)
- **Drag-over affordance.** Today there is zero feedback that the pet is a drop target (`lib.rs:259-288` handles only `Drop`; no `Enter/Over/Leave`, no CSS). Add webview `onDragOver/onDragLeave/onDrop` on `.pet-root` toggling a `.pet-root--drag` highlight (ring/glow + slight scale). The real drop still flows through the existing Tauri `DragDrop` path; the DOM handlers are highlight-only. Clear the highlight on the drop listener too.
- **Privacy disclosure.** Dropped file *contents* (up to 256KB/file) are sent via A2A `message/send` to the agent's model — commonly a **cloud** model — with no disclosure (`pet/mod.rs:434-461`). For a local-first product this is a footgun (`.env` is in the text allowlist). When the resolved model is non-local, the reading bubble / chat draft header states e.g. "Sending file to \<provider>…". No consent dialog (YAGNI) — disclosure only.
- **Drop honesty.** `>5` files: the 6th is silently dropped (`.take(PET_DROP_MAX_FILES)`, `pet/mod.rs:366`); `skipped` collapses three reasons (non-text / unreadable / over-budget) into a bare count. Make the count reflect truncation and surface skipped filenames in the chat draft (already opened), not just a number in the tiny bubble.
- **Non-dismissible pending bubble.** The `Reading…` bubble has a ✕ that doesn't cancel the in-flight dial (`pet/mod.rs:437`, no cancellation token); a fresh result bubble pops after the user dismissed it. Make the pending bubble non-dismissible (spinner, dwell governed by the promise not a timer); clear it when settled. True cancellation deferred. Add a `tokio::time::timeout` (~45s, under the 60s dwell) so a hung runtime yields "(timed out reaching \<agent>)".

## 7. Bubble (glance state) correctness

- **CJK-safe typography** on bubble + panel text: `line-height:1.7`, `text-align:left` (never justify), `max-width:~34em`, `overflow-wrap:anywhere` (replace the legacy `word-break:break-word` at `pet.css:192`, which also fixes long-URL overflow).
- **`document.documentElement.lang = lang`** — `index.html` hardcodes `lang="en"` and nothing syncs it on language switch (`i18n/index.tsx:40`). WebKit uses `lang` to choose Han glyph variants (TC vs SC/JP) and CJK line-breaking. **One line, directly improves the reported CJK rendering.** Font stack is already CJK-capable (PingFang TC / JhengHei, `primitives.css:25`) — no change there.
- **Contrast / hit-targets:** bubble close button resting color → `--text-secondary` (currently `--text-tertiary`, likely fails 4.5:1); nudge close + menu padding toward the 24px floor where it still fits the window.

## 8. Interaction & accessibility

- **Single-click opens the panel** (primary action; research: double-click is "a relic"). Double-click becomes the redundant "expand to full" shortcut. The greeting-wave moves to spawn/hover.
- **Keyboard-operable sprite.** The sprite is a plain `<div>` — no `tabIndex`, `role`, or key handler; the pet is 100% keyboard/VoiceOver-inoperable (`PetApp.tsx:247-254`). Make it a real button: `role="button"`, `tabIndex={0}`, `aria-label={t('pet.chat')}`, `onKeyDown` Enter/Space → open chat. Set `aria-hidden` on the inner `PetFace`/img so the button label isn't double-announced.
- **Escape also closes the context menu** (today it only closes the bubble, `PetApp.tsx:135-143`); focus the first menu item on open. Full arrow-key roving skipped — Tab already moves between the `<button>`s.
- **Mute persistence + indicator.** Mute is React-state only; every reload/respawn/Hide-round-trip silently un-mutes, and the state is invisible (`PetApp.tsx:193`). Persist to `localStorage` keyed by agent; add a 🔇 corner badge on the sprite.

## 9. Mascot liveness, material, lifecycle, deletion

- **Liveness.** 12 expression slots exist but only idle/smile/peek animate; `thinking` (agent working) and `talk_open/close` (streaming) render frozen (`PetFace.tsx:274-275`). Add a subtle pulse while a dial is pending and a ~250ms mouth-flap while a reply streams (driver already sets `expression` via the `pet-expression` event). Honor `prefers-reduced-motion` (global catch-all already covers it; keep new states under it).
- **Material consistency (faux-glass).** Chat uses native HudWindow vibrancy; the bubble + menu are flat opaque `--surface-card` (`pet.css:160-171, 90-100`). A webview can't get NSVisualEffectView, so use CSS faux-glass: `background: color-mix(... 78%, transparent)` + `backdrop-filter: blur(18px) saturate(140%)` + translucent border; the bubble tail must switch to the same translucent fill.
- **Entrance flash.** The pet builds visible immediately; a transparent webview briefly paints an opaque square before React mounts. Add `.visible(false)` + `show()` after build, mirroring `chat_window.rs:47,61`.
- **Hide-1h → Rust timer.** `handleHide1h` uses a JS `setTimeout(...,1h)` that dies on webview reload → pet hidden forever with no decorated UI to recover (`PetApp.tsx:203-209`). Move to a `pet_hide_for(agent, secs)` command that `spawn`s a `sleep().then(show())`, no-op if the pet was closed meanwhile. Drop the JS timer.
- **Deletion (ponytail).** `pet_position.json` is **write-only dead code** — written on every move, never read back; `display_id` is always `None`; the implied "restore on restart" was never built and is intentionally not wanted. Delete `PetPosition`, the file writes, `pet_reposition`, and the `Moved` listener (`pet/mod.rs:30-36,230-239`, `PetApp.tsx:116-120`). Less code, identical behavior.

## 10. Phasing (PR-sized, each independently shippable)

- **Phase 1 — reported bugs + escalation foundation.** Double-jump fix (§5); file-summary → panel (§6 core); panel anchoring + z-order + compact size + expand (§4); pet window enlarge + CJK typography + `html lang` (§4, §7); single-click opens panel (§8). *Ships both screenshots fixed.*
- **Phase 2 — hardening + cheap wins.** Keyboard sprite + Escape-menu + contrast (§8); mute persist + badge (§8); faux-glass + entrance flash (§9); off-screen clamp (§4); Hide-1h → Rust (§9); delete dead `pet_position` (§9).
- **Phase 3 — file-drop UX + liveness.** Drag-over affordance, privacy disclosure, drop honesty, non-dismissible pending bubble + timeout (§6); thinking/talking motion (§9).

## 11. Testing

Per ponytail, one runnable check per piece of non-trivial logic:
- **Rust unit tests** for the pure geometry: panel anchoring (adjacent placement, edge-flip, work-area clamp) and off-screen spawn clamp — table-driven over synthetic monitor rects.
- **Manual / live E2E** for window behavior that needs a real WindowServer: double-click no longer raises the Hub; panel lands next to the pet on the pet's monitor; panel sits above the pet; CJK bubble renders multi-char lines; drag-over highlight; file-drop streams into the panel; mute survives reload.
- **Visual** check of faux-glass in light/dark and the entrance-flash fix.

## 12. Deferred / YAGNI (considered and rejected)

`tauri-nspanel` non-activating panel (only if focus-steal persists); voice; RTL/`dir` support (no RTL locale ships); pet restore-on-restart (intentional "every start is user-initiated"); live display-disconnect handling (macOS reparents + Return-to-Hub covers it); fullscreen-Space float guarantees (needs `NSWindow.collectionBehavior` beyond Tauri); generic pet event-loop reaper (self-heals on respawn via dropped `oneshot::Sender`); per-animation reduced-motion guards (global catch-all already covers them); first-run tutorial (folded into spawn/hover greeting); byte/char unit-mixing in the drop budget (guards different things, harmless).
