# murmur Panel P4 — Recommendations + Live Stream Render Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the Panel's last two phases from the parent spec: context-aware recommendations (skills/workflows for the session's cwd) and gated live agent-stream rendering in the Preview tab.

**Architecture:** Recommendations: a mur-core lib fn (`recommend::recommend_for_cwd`) reusing the retrieve pipeline (`score_and_rank_generic`, same pattern as `cmd/hook.rs:207`), wrapped by hidden CLI `mur internals recommend` and called directly by a Hub Tauri command (P2's established pattern — the Hub depends on mur-core). Stream: an additive `PanelFrame::Stream { delta }` wire variant; murmur forwards its existing `StreamMsg::Delta` events to the `PanelHandle` only when the per-session gate is on (`/panel stream on|off`, default **off**); the Hub republishes to the webview which renders a rolling tail in the Preview tab.

**Tech Stack:** Rust (mur-core retrieve pipeline, mur-common wire types, Tauri), React/TS.

**Spec:** P4 phase of `docs/superpowers/specs/2026-07-05-murmur-panel-companion-design.md` ("recommendations + live stream render — `mur internals recommend`, `stream {delta}` forwarding (experimental, gated)").

**Prerequisites:** P2 plan (Tauri `panel/data.rs` module exists; Information tab renders data) and P3 plan (Preview tab renders; `PreviewPane` exists). Land those first.

## Global Constraints

- Stream forwarding is **default OFF**, per-session, toggled by `/panel stream on|off` — never persisted, never enabled by a Hub-side action (the gate lives murmur-side; insert-only model preserved).
- All new wire variants are additive; P1's tolerant `decode_line` keeps old Hub/TUI pairs working.
- Recommendations are read-only retrieval; clicking one only `insert`s a command.
- Stream deltas are fire-and-forget (`PanelHandle::send` already drops beyond `CHANNEL_CAP=64` when no Hub is connected) — streaming must never block or slow the TUI.
- Frontend stream buffer capped (`STREAM_TAIL_CHARS = 20_000`) — keep the tail, drop the head.
- fmt/clippy/tests green per commit; build env `ORT_STRATEGY=download`, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`.

---

### Task 1: `recommend_for_cwd` in mur-core

**Files:**
- Create: `mur-core/src/recommend.rs`
- Modify: `mur-core/src/lib.rs` (`pub mod recommend;`)

**Interfaces:**
- Consumes: `crate::retrieve::scoring::score_and_rank_generic(query, candidates)`; skill/workflow candidate loading — copy the exact loading calls from `cmd/hook.rs:145-220` (`cmd_hook_prompt`'s candidate assembly; the stores it uses are the source of truth).
- Produces:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct Recommendation {
    pub name: String,
    pub kind: String,      // "skill" | "workflow"
    pub score: f32,
    pub description: String,
    /// Ready-to-edit command for insert-on-click.
    pub command: String,   // "mur run <name>" | "mur skill show <name>"
}

/// Retrieve skills/workflows relevant to a working directory. Query is built
/// from the cwd's trailing path components (project + parent dir names) —
/// the same signal the session itself carries. Fail-soft: errors → empty.
pub fn recommend_for_cwd(cwd: &std::path::Path, limit: usize) -> Vec<Recommendation>
```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_uses_trailing_path_components() {
        assert_eq!(
            cwd_query(std::path::Path::new("/Volumes/x/Projects/mur")),
            "Projects mur"
        );
        assert_eq!(cwd_query(std::path::Path::new("/")), "");
    }

    #[test]
    fn recommend_is_fail_soft_on_empty_home() {
        // No ~/.mur stores reachable in the test env → must return empty, not Err/panic.
        let recs = recommend_for_cwd(std::path::Path::new("/nonexistent/dir"), 5);
        assert!(recs.len() <= 5);
    }
}
```

- [ ] **Step 2: Run, FAIL** — `cargo test -p mur-core recommend`

- [ ] **Step 3: Implement**

```rust
//! Context recommendations for the Panel (P4): skills/workflows relevant to a
//! session cwd, via the standard retrieve pipeline. Read-only, fail-soft.

use std::path::Path;

use crate::retrieve::scoring::score_and_rank_generic;

pub(crate) fn cwd_query(cwd: &Path) -> String {
    cwd.components()
        .rev()
        .take(2)
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn recommend_for_cwd(cwd: &Path, limit: usize) -> Vec<Recommendation> {
    let query = cwd_query(cwd);
    if query.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Skills — load candidates exactly as cmd/hook.rs does (copy its store
    // calls verbatim; adapt if the loader returns Result).
    if let Ok(skills) = crate::store::skills_all_for_retrieval() {
        for s in score_and_rank_generic(&query, skills).into_iter().take(limit) {
            out.push(Recommendation {
                name: s.item.name().to_string(),
                kind: "skill".into(),
                score: s.score,
                description: s.item.description().to_string(),
                command: format!("mur skill show {}", s.item.name()),
            });
        }
    }
    if let Ok(workflows) = crate::store::workflows_all_for_retrieval() {
        for w in score_and_rank_generic(&query, workflows).into_iter().take(limit) {
            out.push(Recommendation {
                name: w.item.name().to_string(),
                kind: "workflow".into(),
                score: w.score,
                description: w.item.description().to_string(),
                command: format!("mur run \"{}\"", w.item.name()),
            });
        }
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.truncate(limit);
    out
}
```

**Implementation note (binding):** `skills_all_for_retrieval` / `workflows_all_for_retrieval` are placeholders for whatever loader `cmd/hook.rs`'s `cmd_hook_prompt` actually calls at lines ~145–210 — open that function and reuse its exact candidate-loading calls (including the `Retrievable` accessor names for name/description on `Scored<T>`); the min-score floor (`retrieval.min_score`, 0.42) applies the same way it does there. The tests + the `Recommendation` shape are the contract.

- [ ] **Step 4: Run, PASS** — `cargo test -p mur-core recommend`

- [ ] **Step 5: fmt/clippy + Commit**

```bash
git add mur-core/src/recommend.rs mur-core/src/lib.rs
git commit -m "feat(recommend): cwd-based skill/workflow recommendations (retrieve pipeline)"
```

---

### Task 2: Hidden CLI `mur internals recommend`

**Files:**
- Modify: `mur-core/src/cli/actions.rs` (`InternalsAction` — beside P2's `ScheduleStatus` variant)
- Modify: `mur-core/src/dispatch.rs` (dispatch arm beside `InternalsAction::ScheduleStatus`)

**Interfaces:**
- Consumes: `crate::recommend::recommend_for_cwd` (Task 1).
- Produces: `mur internals recommend --cwd <path> [--limit N]` printing `{"recommendations": [...]}` JSON (always JSON; spec's `--json` implicit, matching P2's `schedule-status` precedent).

- [ ] **Step 1: Add variant**

```rust
    /// Context recommendations for a working directory as JSON — Panel data source
    #[command(hide = true)]
    Recommend {
        /// Working directory to recommend for
        #[arg(long)]
        cwd: String,
        /// Max items
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
```

- [ ] **Step 2: Dispatch arm**

```rust
    InternalsAction::Recommend { cwd, limit } => {
        let recs = crate::recommend::recommend_for_cwd(std::path::Path::new(&cwd), limit);
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "recommendations": recs }))?);
    }
```

- [ ] **Step 3: Manual verify** — `cargo run -p mur-core --bin mur -- internals recommend --cwd $(pwd)` → JSON with a `recommendations` array (env vars per Global Constraints).

- [ ] **Step 4: fmt/clippy + Commit**

```bash
git add mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): hidden 'mur internals recommend' JSON command"
```

---

### Task 3: `Stream` wire variant + gated murmur forwarding

**Files:**
- Modify: `mur-common/src/panel.rs` (`PanelFrame::Stream`)
- Modify: `mur-core/src/cmd/agent/cli/panel.rs` (`/panel stream on|off` handling — the `/panel` subcommand match at ~line 183)
- Modify: `mur-core/src/cmd/agent/cli/app.rs` (gate field + forward on `StreamMsg::Delta`)
- Modify: `mur-core/src/cmd/agent/cli/complete.rs` (add `stream` to `/panel` candidates)

**Interfaces:**
- Consumes: `PanelHandle::send(frame)` (fire-and-forget, P1); `StreamMsg::Delta { task_id, text, .. }` handling in `app.rs` (find where the TUI appends deltas to the streaming bubble — grep `StreamMsg::Delta` in `app.rs`/`mod.rs`).
- Produces: `PanelFrame::Stream { delta: String }` serializing as `{"type":"stream","delta":"…"}`; app-state field `pub panel_stream: bool` (default `false`).

- [ ] **Step 1: Failing test** (extend `frames_round_trip` in `mur-common/src/panel.rs`)

```rust
        let line = serde_json::to_string(&PanelFrame::Stream { delta: "tok".into() }).unwrap();
        assert!(line.contains("\"type\":\"stream\""));
        assert!(matches!(
            decode_line::<PanelFrame>(&line),
            Some(PanelFrame::Stream { delta }) if delta == "tok"
        ));
```

- [ ] **Step 2: Run, FAIL** — `cargo test -p mur-common panel`

- [ ] **Step 3: Implement.**
  - `panel.rs` (mur-common): add `Stream { delta: String },` to `PanelFrame` with doc comment `/// Live agent-output delta (P4; sent only while the session gate is on).`
  - `app.rs`: add `pub panel_stream: bool` to the app state struct (init `false` beside `panel: None` at ~line 362).
  - `cli/panel.rs`: in the `/panel` subcommand match, add:

```rust
        Some("stream") => match args.get(1).map(String::as_str) {
            Some("on") => { app.panel_stream = true; /* + a one-line system notice "panel stream on" via the existing notice helper */ }
            Some("off") => { app.panel_stream = false; /* notice "panel stream off" */ }
            _ => { /* usage notice: "/panel stream on|off" */ }
        },
```

  (Use the same notice/system-line helper the surrounding arms use — copy the adjacent `preview` arm's style. Update the `usage:` string at line 167 to `[information|activities|preview|notifications|schedule] · /panel preview <path|url> · /panel stream on|off`.)
  - At the `StreamMsg::Delta` handling site in `app.rs` (where `text` is appended to the streaming bubble), add:

```rust
        if self.panel_stream
            && let Some(panel) = &self.panel
        {
            panel.send(mur_common::panel::PanelFrame::Stream { delta: text.clone() });
        }
```

  - `complete.rs`: add `"stream"` beside the tab candidates for `/panel`.

- [ ] **Step 4: Run, PASS** — `cargo test -p mur-common panel && cargo test -p mur-core panel` (also extend the existing `parses_panel` test in `app.rs:921` with `/panel stream on` if the parse layer needs a case).

- [ ] **Step 5: fmt/clippy + Commit**

```bash
git add mur-common/src/panel.rs mur-core/src/cmd/agent/cli/
git commit -m "feat(panel): gated stream{delta} forwarding (/panel stream on|off, default off)"
```

---

### Task 4: Hub — republish stream + recommendations command

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/panel/mod.rs` (add a `PanelFrame::Stream` arm beside the `Preview` arm at ~line 81, emitting `panel-stream` with `{ pid, delta }`)
- Modify: `mur-hub-gui/src-tauri/src/panel/data.rs` (add `panel_recommend`)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` + `capabilities/panel.json` (register `panel_recommend`; allow `panel-stream` event if events are capability-listed like `panel-preview` is)
- Modify (if needed): `mur-gui-core/src/panel_bridge/client.rs` — check whether the bridge forwards frames generically or matches variants; if it matches, add the `Stream` arm (forward as-is on the EventBus).

**Interfaces:**
- Consumes: Task 1's `recommend_for_cwd`; Task 3's `Stream` frame.
- Produces: Tauri command `panel_recommend(cwd: String) -> Vec<Recommendation>`; webview event `panel-stream` payload `{ pid: u32, delta: String }`.

- [ ] **Step 1: Implement**

```rust
// panel/data.rs
#[tauri::command]
pub fn panel_recommend(cwd: String) -> Vec<mur_core::recommend::Recommendation> {
    mur_core::recommend::recommend_for_cwd(std::path::Path::new(&cwd), 5)
}
```

`panel/mod.rs` — mirror the `Preview` arm:

```rust
        PanelFrame::Stream { delta } => {
            let _ = app.emit("panel-stream", serde_json::json!({ "pid": pid, "delta": delta }));
        }
```

(match the emit style/target of the surrounding arms exactly — P1 may use `emit_to` with the panel window label.)

- [ ] **Step 2: Build check** — `cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml`

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/src-tauri/src/ mur-hub-gui/src-tauri/capabilities/panel.json mur-gui-core/src/panel_bridge/
git commit -m "feat(hub): panel-stream republish + panel_recommend command"
```

---

### Task 5: Frontend — recommendations list + stream tail

**Files:**
- Modify: `mur-hub-gui/ui/src/components/panel/PanelWindow.tsx` (Information tab: recommendations block; Preview tab: stream mode)
- Modify: `mur-hub-gui/ui/src/components/panel/PreviewPane.tsx` (P3) or a sibling `StreamTail.tsx`
- Modify: `mur-hub-gui/ui/src/components/panel/panel.css`

**Interfaces:**
- Consumes: `panel_recommend`, `panel-stream` event (Task 4), `panel_insert` (P1).

- [ ] **Step 1: Recommendations in Information tab.** Fetch on session change/focus:

```tsx
type Recommendation = { name: string; kind: "skill" | "workflow"; score: number; description: string; command: string };
// invoke<Recommendation[]>("panel_recommend", { cwd: sess.cwd })
```

Render as a "Recommended" list under the git/cost rows; each row: kind badge, name, description (truncated), click → `invoke("panel_insert", { pid: sess.pid, text: r.command })`. This replaces the P1 demo-insert affordance permanently.

- [ ] **Step 2: Stream tail.** New `StreamTail.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

const STREAM_TAIL_CHARS = 20_000;

export default function StreamTail({ pid }: { pid: number }) {
  const [buf, setBuf] = useState("");
  const ref = useRef<HTMLPreElement>(null);
  useEffect(() => {
    setBuf("");
    const un = listen<{ pid: number; delta: string }>("panel-stream", (e) => {
      if (e.payload.pid !== pid) return;
      setBuf((b) => (b + e.payload.delta).slice(-STREAM_TAIL_CHARS));
    });
    return () => { un.then((f) => f()); };
  }, [pid]);
  useEffect(() => { ref.current?.scrollTo(0, ref.current.scrollHeight); }, [buf]);
  return buf ? (
    <pre ref={ref} className="stream-tail">{buf}</pre>
  ) : (
    <p className="panel-empty">
      No live stream — type <code>/panel stream on</code> in murmur (experimental).
    </p>
  );
}
```

Preview tab arm: when there's no preview target, or via a small "Live" sub-toggle in the Preview tab header, show `<StreamTail pid={sess.pid} />`. Keep it simple: sub-toggle with two options `Target` / `Live`, defaulting to `Target` when a preview target exists, else `Live`.

- [ ] **Step 3: Build** — `cd mur-hub-gui/ui && npm run build`

- [ ] **Step 4: Manual verify** — Hub `.app` + `murmur`:
  - Information tab shows up to 5 recommendations for the repo cwd; click inserts the command into murmur's input.
  - `/panel stream on` → send a message to the agent → deltas appear live in Preview/Live; `/panel stream off` stops them; fresh sessions default off.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/panel/
git commit -m "feat(hub-ui): Panel P4 — recommendations + live stream tail"
```

---

### Task 6: Green + docs

- [ ] **Step 1:** `cargo fmt --all` (+ excluded Tauri crates), clippy workspace + hub manifest, `cargo nextest run -p mur-core -p mur-common` (or `cargo test`), `npm run build`.
- [ ] **Step 2:** Update `docs/architecture/runtime-overview.md` panel section (P4 shipped: recommendations, `/panel stream`); mark the parent spec's phasing list P1–P4 complete.
- [ ] **Step 3: Commit** — `git add -A && git commit -m "docs(panel): P4 shipped — Panel phases complete"`

---

## Self-Review

**Spec coverage (parent spec P4):** `mur internals recommend` reusing `score_and_rank_generic` ✓ (Tasks 1–2; also exposed as a direct lib call for the Hub per the P2 pattern), clicking a recommendation inserts the ready-to-edit command ✓ (Task 5), `stream {delta}` forwarding gated + experimental ✓ (Task 3 — default off, per-session, murmur-side gate), live render ✓ (Tasks 4–5), "built last" ✓ (this is the final phase; prerequisites P2+P3 stated).

**Placeholders:** Task 1's store-loader names are explicitly bound to `cmd/hook.rs`'s real calls (flagged as the source of truth) — the one intentional adaptation point, same convention the P2 plan used for `AgentProfile.lifecycle`.

**Type consistency:** `Recommendation` shape identical across lib fn, CLI JSON, Tauri command, and TS type. `PanelFrame::Stream { delta: String }` consistent across mur-common test, app.rs forward, Hub arm, and TS payload `{pid, delta}` (pid added at the Hub republish layer, matching the `panel-preview` event's shape).

**Gate audit (feedback_autonomous_loop_safety_audit):** stream gate is opt-in per session, default off, controlled only from the terminal the user is sitting at; Hub cannot enable it (no Hub→murmur frame other than `insert` exists, and `insert` only fills the input box).
