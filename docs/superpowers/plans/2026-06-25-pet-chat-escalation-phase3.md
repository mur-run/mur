# Pet Chat Escalation — Phase 3 Implementation Plan (file-drop UX + liveness)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make file-drop a first-class chat interaction (summary streams into the panel, not the cramped bubble), disclose when file contents leave for a cloud model, give the drop a visible target, make truncation honest, and bring the mascot to life while it works.

**Architecture:** The pet drop currently does an **ephemeral** `message/send` dial whose exchange is never persisted, so it can't reach the chat panel. Phase 3 routes the exchange into the agent's channel via `mur_core::mobile::persist_mobile_exchange` — the existing `channel-updated` watcher (`lib.rs:502-515`) then re-hydrates `ChatTab` (which loads via `channel_load`), so the dropped file + agent take appear as real messages. The rest is UI affordance (drag highlight, mascot motion) + safety (privacy disclosure, honest truncation). Branch `feat/pet-chat-escalation`; base = Phase 2 head.

**Tech Stack:** Rust + Tauri 2 (`mur-hub-gui/src-tauri`, `mur-core`), React 18 + TS + Vite (`mur-hub-gui/ui`).

## Global Constraints

- `cargo` not on PATH: prepend `export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`.
- `mur-hub-gui` workspace-EXCLUDED: `--manifest-path mur-hub-gui/src-tauri/Cargo.toml`; bin `mur-hub-gui`. Frontend: `cd mur-hub-gui/ui && npm run build`.
- Stage ONLY each task's named files — NEVER `git add -A`. Leave unrelated tree changes (`.mcp.json`, `Cargo.lock`) alone.
- Never delete files / run disk cleanup; build fails on disk → stop and report.
- Brand "MUR" uppercase in user-visible strings; reply/UX copy supports zh-TW (add both en + zh-TW i18n keys for any new user-visible string — the i18n table is type-checked for parity).
- Line numbers indicative — grep/read the current file before editing.
- `pet_drop_files` already has safety caps (`PET_DROP_MAX_FILES=5`, `PET_DROP_MAX_TOTAL_BYTES`, `PET_DROP_READ_BYTE_CAP`, char truncation) — preserve them; do not weaken the bounded-read protections.

---

### Task 1: Route the dropped-file exchange into the agent's channel

So the take appears in the chat panel as a real message instead of being trapped in the bubble.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs` (`pet_drop_files`)

**Interfaces:**
- Consumes: `mur_core::mobile::persist_mobile_exchange` (verify exact name/signature in `mur-core/src/mobile.rs` ~561-598 before calling — it appends a user+agent turn pair to the agent's latest channel, best-effort).
- Consumes: the existing `channel-updated` watcher → `ChatTab` `channel_load(name)` re-hydration (no frontend change needed).

- [ ] **Step 1: Confirm the persist API + that channel_load reads the same channel**

```bash
export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
grep -n "pub fn persist_mobile_exchange" mur-core/src/mobile.rs
grep -n "pub fn persist_exchange\|latest_for_agent\|channel_load" mur-hub-gui/src-tauri/src/chat.rs mur-hub-gui/src-tauri/src/*.rs
```
Confirm: (a) the public `persist_mobile_exchange` signature (args: home, agent, user_text, reply — adapt to the real one); (b) it writes to the agent's latest channel; (c) `channel_load` (the command `ChatTab` calls) reads that same latest channel. If `persist_mobile_exchange` is not public or writes elsewhere, prefer `chat::persist_exchange` (chat.rs ~257-292) if callable, or report the mismatch. Do NOT proceed on a guess about which channel is read vs written.

- [ ] **Step 2: Persist the exchange in `pet_drop_files`**

After the inline take `reply` is computed (the `spawn_blocking` dial result), persist the exchange so it lands in the channel. Build a user message that references the dropped files (the "chip" is a 📎 filename prefix — a styled chip is deferred, YAGNI):
```rust
    // Make the dropped-file exchange a real channel turn so it shows in the
    // chat panel (channel-updated → ChatTab re-hydrates), not just the bubble.
    let file_names: Vec<String> = sections
        .iter()
        .filter_map(|s| s.lines().next().map(|l| l.trim_start_matches("=== ").trim_end_matches(" ===").to_string()))
        .collect();
    let user_msg = format!("📎 Dropped: {}\n\n{body}", file_names.join(", "));
    let home2 = mur_home();
    let agent2 = agent_name.clone();
    let reply2 = reply.clone();
    let _ = tokio::task::spawn_blocking(move || {
        mur_core::mobile::persist_mobile_exchange(&home2, &agent2, &user_msg, &reply2)
    })
    .await;
```
(Adapt the call to the real signature from Step 1. `persist_mobile_exchange` is best-effort/no-op on error — fine.) Keep the existing `pet-open-chat` emit so the panel opens; the panel then shows the persisted exchange.

- [ ] **Step 3: Reduce the bubble to a short pointer (the take now lives in the panel)**

The full take no longer needs to fill the bubble. Change `pet_drop_files`'s `PetDropResult.reply` (shown in the bubble by `PetApp.tsx`) to a brief confirmation instead of the full multi-sentence take — e.g. return the take as before BUT the frontend bubble should show a short form. Simplest: keep returning `reply` (so a glance is available) but ALSO ensure it's short; the panel has the full version. (If `reply` is already 1-2 sentences per the prompt, leave it — the readable bubble from Phase 1 is fine as a glance; the durable copy is in the panel.) No frontend change required here if the bubble already shows the 1-2 sentence take.

- [ ] **Step 4: Build + commit**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
git add mur-hub-gui/src-tauri/src/pet/mod.rs
git commit -m "feat(pet): persist dropped-file exchange to channel (shows in chat panel)"
```

- [ ] **Step 5: Live check (operator/computer-use)**

Drop a text file on a pet → the chat panel opens → within ~1-2s the file message + agent take appear in the panel as a message pair. (Needs a running runtime for the agent.)

---

### Task 2: Privacy disclosure when file contents leave for a non-local model

Dropped file contents (up to 256KB) are sent to the agent's model — often cloud. Disclose it.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs` (resolve the agent's model provider; return a `cloud` flag or provider name in `PetDropResult`)
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (the "reading…" bubble copy)
- Modify: `mur-hub-gui/ui/src/i18n/en.ts` + `zh-TW.ts` (disclosure string)

- [ ] **Step 1: Find an existing "is this model local?" check**

```bash
grep -rn "is_local\|local_model\|provider\|localhost\|127.0.0.1\|ollama\|mlx\|lmstudio" mur-core/src/ mur-hub-gui/src-tauri/src/ | grep -i "model\|provider\|local" | head -30
```
Determine how an agent's resolved model + provider is read (agent profile → model alias → `~/.mur/models.yaml` provider; "local" = provider points at a localhost/loopback base URL or a known local runtime). If a reusable helper exists, use it. If not, the FLOOR is a generic disclosure (Step 3) — do not build a model registry from scratch (YAGNI).

- [ ] **Step 2: Surface a provider/cloud signal from the drop**

Resolve the agent's model provider in `pet_drop_files` (before/around the dial) and add a field to `PetDropResult`, e.g. `pub remote_provider: Option<String>` (Some("Anthropic"/"OpenAI"/…) when non-local, None when local/unknown-local). Use the helper from Step 1; if none exists, set `remote_provider` to `Some("the agent's model")` whenever the model isn't demonstrably local (fail-toward-disclosure).

- [ ] **Step 3: Disclose in the reading bubble**

In `PetApp.tsx`, the `pet://drop` handler sets the "reading…" bubble. When `remote_provider` is present (returned in `PetDropResult`), the bubble (or the result bubble) includes a disclosure, e.g. `t("pet.dropSendingTo", { provider })`. Add i18n keys to BOTH locales:
- en: `"pet.dropSendingTo": "Sending file contents to {provider}…"`
- zh-TW: `"pet.dropSendingTo": "正在把檔案內容傳給 {provider}…"`
No consent dialog (YAGNI) — disclosure only.

- [ ] **Step 4: Build + commit**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cd mur-hub-gui/ui && npm run build && cd -
git add mur-hub-gui/src-tauri/src/pet/mod.rs mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "feat(pet): disclose when dropped file contents go to a non-local model"
```

---

### Task 3: Drag-over drop affordance

Today there is zero feedback that the pet is a drop target. Add a highlight.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (drag handlers on `.pet-root`)
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (`.pet-root--drag` style)

- [ ] **Step 1: Webview drag handlers (highlight only; the real drop still flows through the Tauri `pet://drop` path)**

Add local state `const [dragOver, setDragOver] = useState(false)` and on the `.pet-root` div:
```tsx
      onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
      onDragLeave={() => setDragOver(false)}
      onDrop={() => setDragOver(false)}
```
Apply the class: `className={`pet-root${dragOver ? " pet-root--drag" : ""}`}`. Also clear it in the existing `pet://drop` listener (in case `dragleave` doesn't fire): call `setDragOver(false)` at the top of that handler.

- [ ] **Step 2: Highlight style**

In `pet.css`:
```css
.pet-root--drag .pet-sprite {
  transform: scale(1.08);
  filter: drop-shadow(0 0 0 2px var(--color-brand)) drop-shadow(0 4px 12px rgba(0,0,0,0.25));
}
@media (prefers-reduced-motion: reduce) {
  .pet-root--drag .pet-sprite { transform: none; }
}
```

- [ ] **Step 3: Build + commit**

```bash
cd mur-hub-gui/ui && npm run build
git add mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/styles/components/pet.css
git commit -m "feat(pet): drag-over drop-target highlight"
```

---

### Task 4: Honest truncation + non-dismissible pending + dial timeout

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/pet/mod.rs` (`pet_drop_files`)
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (pending bubble)

- [ ] **Step 1: Signal >5-file truncation + surface skipped names**

In `pet_drop_files`, the `paths.iter().take(PET_DROP_MAX_FILES)` silently drops files 6+. Before/after the loop, if `paths.len() > PET_DROP_MAX_FILES`, push a synthetic entry into `skipped` (e.g. format!("+{} more (max {})", paths.len() - PET_DROP_MAX_FILES, PET_DROP_MAX_FILES)). Ensure `skipped` filenames (not just a count) reach the user: include them in the persisted channel user message (Task 1's `user_msg`) so they land in the panel, in addition to the bubble's count.

- [ ] **Step 2: Bounded dial (timeout) so a hung runtime can't hang the bubble forever**

Wrap the `spawn_blocking` dial join in a timeout, returning a clear message on elapse:
```rust
    let dialed = match tokio::time::timeout(
        std::time::Duration::from_secs(45),
        tokio::task::spawn_blocking(move || { /* existing dial */ }),
    ).await {
        Ok(join) => join.map_err(|e| e.to_string())?,
        Err(_) => Ok("(timed out reaching the agent)".to_string()),
    };
```
(45s is under the 60s bubble dwell. Adapt to the existing dial code shape.)

- [ ] **Step 3: Non-dismissible pending bubble**

In `PetApp.tsx`, while the `pet_drop_files` promise is in flight, the "reading…" bubble's ✕ shouldn't pretend to cancel (the dial keeps running). Track a `pending` boolean (set true before `invoke`, false in `.then/.catch/.finally`). Pass it to the `Bubble` so the close button is hidden (or disabled) while `pending`. Minimal: in the `pet://drop` handler set a `pending` state; render the bubble's close button only when `!pending`.

- [ ] **Step 4: Build + commit**

```bash
cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cd mur-hub-gui/ui && npm run build && cd -
git add mur-hub-gui/src-tauri/src/pet/mod.rs mur-hub-gui/ui/src/components/PetApp.tsx
git commit -m "feat(pet): honest drop truncation, dial timeout, non-dismissible pending bubble"
```

---

### Task 5: Thinking mascot state while the drop dial runs

12 expression slots exist but only idle/smile/peek animate; a working agent looks frozen.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/PetApp.tsx` (set a "thinking" expression while pending)
- Modify: `mur-hub-gui/ui/src/components/PetFace.tsx` (animate non-calm states; add a thinking pulse)
- Modify: `mur-hub-gui/ui/src/styles/components/pet.css` (`thinking` pulse keyframe, reduced-motion guarded)

- [ ] **Step 1: Drive a thinking state from the drop pending flag**

In `PetApp.tsx`, while the drop dial is pending (the `pending` flag from Task 4), set the local `expression` to `"think"` (or the existing thinking slot name — check `EXPR` in `PetFace.tsx` for the exact key), reverting to `"idle"` when done. Do not fight the backend `pet-expression` listener — gate the local override so the pending state wins only while pending.

- [ ] **Step 2: Make the thinking state visibly animate**

In `PetFace.tsx`, the breathe/blink classes are gated to calm states (idle|smile|peek). Add the `think` state to the set that keeps blinking, and add a subtle pulse. In `pet.css`:
```css
.petface--thinking .petface__body {
  transform-box: fill-box;
  transform-origin: center bottom;
  animation: petfaceThink 1.1s ease-in-out infinite;
}
@keyframes petfaceThink {
  0%, 100% { transform: scale(1); opacity: 1; }
  50% { transform: scale(1.03); opacity: 0.85; }
}
@media (prefers-reduced-motion: reduce) {
  .petface--thinking .petface__body { animation: none; }
}
```
Wire the `petface--thinking` class in `PetFace.tsx` when `expression === "think"`. (Talk/mouth-flap while the reply streams is deferred — the pet window does not receive the chat-delta stream; revisit if pet-side streaming is added.)

- [ ] **Step 2b: Self-check for the gating logic**

Add a tiny assertion-style check (or a focused manual verification note) that the local thinking override reverts to the backend expression when not pending — a branch that, if broken, would leave the pet stuck "thinking". Manual: drop a file → mascot pulses while reading → returns to idle after the take arrives.

- [ ] **Step 3: Build + commit**

```bash
cd mur-hub-gui/ui && npm run build
git add mur-hub-gui/ui/src/components/PetApp.tsx mur-hub-gui/ui/src/components/PetFace.tsx mur-hub-gui/ui/src/styles/components/pet.css
git commit -m "feat(pet): thinking mascot state while the drop dial runs"
```

---

### Task 6: Verify + lint + live E2E

- [ ] **Step 1: clippy + fmt**

```bash
export PATH="/Volumes/Firecuda4tb/.relocated-home/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
cargo clippy --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml -- --check
cd mur-hub-gui/ui && npm run build && cd -
```
Fix any warnings in touched files (let-chains for nested if-let), commit fmt fixups (touched Rust files only).

- [ ] **Step 2: Live E2E (operator/computer-use; needs a running agent runtime)**

- [ ] Drag a file toward the pet → pet highlights (drop-target affordance).
- [ ] Drop a Chinese text file → mascot pulses "thinking"; bubble shows readable "reading…" then a short take; the **chat panel shows the file message + agent take as messages** (Task 1).
- [ ] If the agent's model is non-local → the bubble discloses the provider (Task 2).
- [ ] Drop >5 files → the count + skipped names are honest (in the panel message).
- [ ] While reading, the bubble ✕ is hidden (can't fake-cancel).

## Self-Review

- §6 file-summary → panel (chip + take as messages) → Task 1 (+ 📎 filename "chip"; styled chip deferred YAGNI). ✓
- §6 privacy disclosure (file → non-local model) → Task 2. ✓
- §6 drag-over affordance → Task 3. ✓
- §6 drop honesty (>5 files, skipped names) → Task 4 Step 1. ✓
- §6 non-dismissible pending + dial timeout → Task 4 Steps 2-3. ✓
- §9 thinking mascot motion → Task 5 (talk-while-streaming explicitly deferred — pet lacks the stream signal). ✓
- Integration risk surfaced, not hidden: Task 1 Step 1 + Task 2 Step 1 are real-code verification steps with explicit stop conditions (don't-guess), because the channel-write-vs-read pairing and the local-model helper must be confirmed against current code. These are verification gates, not placeholders.
- i18n parity: every new user-visible string (Task 2) adds both en + zh-TW keys.
- Bounded-read safety caps preserved (Global Constraints + Task 4 wording).
