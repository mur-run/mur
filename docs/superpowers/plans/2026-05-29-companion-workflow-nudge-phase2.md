# Companion Workflow Nudge — Phase 2 (Companion Surface) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Surface Phase 1's pending workflow nudges as proactive companion speech bubbles with Save / Not now / No thanks, routing the user's response back into the Phase-1 ledger (accept → draft workflow) — turning the CLI-only loop into the companion experience.

**Architecture:** Reuse the companion **inbox** message format + **ack/response** plumbing, but **bypass the proactive outbox** (the `run_tick` picker/LLM/rhythm path is for time-of-day chatter, not event-triggered deterministic nudges). At session-end `mur-core` writes a deterministic nudge `.md` to each companion-enabled agent's inbox; the GUI displays it (gating on OS DND, which the GUI layer already has) with action buttons that write a response via the existing `companion_ack`; a `mur-core` drain reads the resulting `UserSignal` events and applies the decision through the Phase-1 `NudgeEmitter`.

**Tech Stack:** Rust 2024 (`mur-core`, `mur-common`, `mur-gui-core`), `serde`; plus a thin TypeScript/React button layer in `mur-hub-gui` (Tauri).

**Depends on:** **Phase 1** (`docs/superpowers/plans/2026-05-29-companion-workflow-nudge-phase1.md`) must be merged — this plan uses `WorkflowCandidate`, `NudgeLedger`, `NudgeState`, `NudgeEmitter`, `NudgeDecision`, `create_draft_workflow`, and the `~/.mur/nudges.json` ledger it introduces.

**Architecture decisions (resolved — see the chat preamble for rationale):**
1. Bypass the outbox; write the nudge inbox `.md` directly from `mur-core`.
2. Nudge engine stays in `mur-core`; DND gating happens in the GUI display layer; no `mur-agent-runtime` changes; no type-move to `mur-common` (types-only crate).
3. Accept→draft happens in a `mur-core` drain of `nudge:*` `UserSignal` events.
4. Deliver to every companion-enabled agent's inbox; none → CLI-only fallback.

**Spec:** `docs/superpowers/specs/2026-05-29-companion-workflow-nudge-design.md` (§4 unit 4).

---

## File Structure

- Modify `mur-common/src/companion/mod.rs` — add `Situation::WorkflowNudge`.
- Create `mur-core/src/nudge/companion.rs` — deterministic nudge body + inbox writer + agent discovery + the response drain. (Keeps `nudge/` cohesive and files focused.)
- Modify `mur-core/src/nudge/mod.rs` — `pub mod companion;` + re-exports.
- Modify `mur-core/src/cmd/session.rs` — after Phase-1 `record_nudges_for_candidates`, deliver inbox messages.
- Modify `mur-core/src/cmd/workflow.rs` — `cmd_suggest` calls the response drain before listing.
- Modify `mur-common/src/config.rs` — flip `NudgeConfig::enabled` default to `true`.
- Modify `mur-gui-core/src/companion_bridge/event.rs` — accept a `snooze` response value.
- Modify `mur-hub-gui/src-tauri/src/companion.rs` — accept `snooze` in `companion_ack` validation; React buttons (TS) for nudge messages.

---

## Task 1: `Situation::WorkflowNudge` (mur-common)

**Files:**
- Modify: `mur-common/src/companion/mod.rs:29-34`
- Test: same file (inline)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn workflow_nudge_situation_serializes_snake_case() {
    assert_eq!(
        serde_json::to_string(&Situation::WorkflowNudge).unwrap(),
        "\"workflow_nudge\""
    );
    let back: Situation = serde_json::from_str("\"workflow_nudge\"").unwrap();
    assert_eq!(back, Situation::WorkflowNudge);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common workflow_nudge_situation`
Expected: FAIL — no variant `WorkflowNudge`.

- [ ] **Step 3: Add the variant**

In `mur-common/src/companion/mod.rs`, extend the enum:

```rust
pub enum Situation {
    MorningGreeting,
    GentleCheckIn,
    ShareQuote,
    ShareLink,
    WorkflowNudge,
}
```

If any code `match`es `Situation` exhaustively (e.g. `situations::weights_by_hour`), add a `WorkflowNudge => 0.0` weight arm (the nudge is never picked by the proactive rhythm — it is delivered out-of-band). Grep `match` on `Situation` and fix non-exhaustive errors with a zero/neutral arm.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common workflow_nudge_situation` and `cargo build -p mur-common -p mur-agent-runtime`
Expected: PASS; both crates compile (exhaustive matches handled).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/companion/mod.rs
git commit -m "feat(nudge): Situation::WorkflowNudge variant"
```

---

## Task 2: Deterministic nudge body (mur-core)

**Files:**
- Create: `mur-core/src/nudge/companion.rs`
- Modify: `mur-core/src/nudge/mod.rs` (add `pub mod companion;`)
- Test: `mur-core/src/nudge/companion.rs` (inline)

> The body is deterministic (not LLM-composed). Keep i18n minimal: an English default now; the locale arg lets Phase-3 add translations without signature change.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;

    fn cand() -> WorkflowCandidate {
        WorkflowCandidate {
            id: "abc123".into(),
            title: "Run tests, then commit, then push".into(),
            suggested_name: "test-commit-push".into(),
            steps_preview: vec!["cargo test".into(), "git commit".into()],
            session_count: 4,
            evidence_session_ids: vec![],
        }
    }

    #[test]
    fn body_mentions_title_and_count() {
        let b = nudge_body(&cand(), "en");
        assert!(b.contains("Run tests, then commit, then push"));
        assert!(b.contains("4")); // session count
    }

    #[test]
    fn message_id_encodes_candidate_id() {
        assert_eq!(nudge_msg_id("abc123"), "nudge:abc123");
        assert_eq!(candidate_id_from_msg("nudge:abc123"), Some("abc123".to_string()));
        assert_eq!(candidate_id_from_msg("other"), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::companion`
Expected: FAIL — `nudge_body` undefined.

- [ ] **Step 3: Implement**

`mur-core/src/nudge/companion.rs` (top):

```rust
use crate::nudge::candidate::WorkflowCandidate;

/// Message id namespace so the response drain can recognize nudge replies.
pub const NUDGE_ID_PREFIX: &str = "nudge:";

pub fn nudge_msg_id(candidate_id: &str) -> String {
    format!("{NUDGE_ID_PREFIX}{candidate_id}")
}

pub fn candidate_id_from_msg(msg_id: &str) -> Option<String> {
    msg_id.strip_prefix(NUDGE_ID_PREFIX).map(|s| s.to_string())
}

/// Deterministic, locale-aware nudge body. English default for v1.
pub fn nudge_body(c: &WorkflowCandidate, _locale: &str) -> String {
    format!(
        "I noticed you did this across {} sessions:\n\n  {}\n\nWant me to save it as a replayable workflow you can run with `mur run {}`?",
        c.session_count, c.title, c.suggested_name
    )
}
```

Add `pub mod companion;` to `mur-core/src/nudge/mod.rs`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::companion`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/companion.rs mur-core/src/nudge/mod.rs
git commit -m "feat(nudge): deterministic companion body + id namespace"
```

---

## Task 3: Inbox writer (mur-core)

**Files:**
- Modify: `mur-core/src/nudge/companion.rs`
- Test: `mur-core/src/nudge/companion.rs` (inline)

> Mirror the inbox `.md` format that `mur-agent-runtime/src/companion/inbox.rs::write_inbox_md` produces and that `mur-core/src/cmd/agent_companion/inbox.rs::ack_at` reads: YAML frontmatter (`id`, `situation`, `template_id`, `locale`, `generated_at`) + body + a trailing `>>> response: <unset>` line. **Open both files and match the exact frontmatter keys/format byte-for-byte** so the GUI parser and `ack_at` accept it. Use `create_new` semantics (skip if the file already exists) for idempotency.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn writes_nudge_inbox_md_with_response_marker() {
    let dir = tempfile::tempdir().unwrap();
    let inbox = dir.path().join("inbox");
    std::fs::create_dir_all(&inbox).unwrap();
    let c = /* cand() from Task 2 */;
    let path = write_nudge_inbox(&inbox, &c, "en").unwrap();
    let s = std::fs::read_to_string(&path).unwrap();
    assert!(s.contains("situation: workflow_nudge"));
    assert!(s.contains("id: nudge:abc123"));
    assert!(s.trim_end().ends_with(">>> response: <unset>"));
    // idempotent: second write for same id is a no-op (already exists)
    assert!(write_nudge_inbox(&inbox, &c, "en").is_ok());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::companion::tests::writes_nudge_inbox`
Expected: FAIL — `write_nudge_inbox` undefined.

- [ ] **Step 3: Implement**

```rust
use std::path::{Path, PathBuf};

/// Write a nudge as a companion inbox `.md` (same format as the runtime's
/// write_inbox_md). Returns the path. No-op (Ok) if a file for this id exists.
pub fn write_nudge_inbox(inbox_dir: &Path, c: &WorkflowCandidate, locale: &str) -> anyhow::Result<PathBuf> {
    let id = nudge_msg_id(&c.id);
    // file name must avoid the ':' in the id on disk — sanitize like the runtime does
    let file = inbox_dir.join(format!("{}.md", id.replace(':', "_")));
    if file.exists() {
        return Ok(file);
    }
    let generated_at = chrono::Utc::now().to_rfc3339();
    let body = nudge_body(c, locale);
    let content = format!(
        "---\nid: {id}\nsituation: workflow_nudge\ntemplate_id: nudge\nlocale: {locale}\ngenerated_at: {generated_at}\n---\n\n{body}\n\n>>> response: <unset>"
    );
    let tmp = file.with_extension("md.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &file)?;
    Ok(file)
}
```

Adjust the exact frontmatter to match `write_inbox_md` precisely (key order, any extra fields, whether it uses `create_new`). If the runtime writer emits additional keys, include them.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::companion::tests::writes_nudge_inbox`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/companion.rs
git commit -m "feat(nudge): write nudge to companion inbox (.md)"
```

---

## Task 4: Deliver at session-end to companion-enabled agents (mur-core)

**Files:**
- Modify: `mur-core/src/nudge/companion.rs` (agent discovery + deliver loop)
- Modify: `mur-core/src/cmd/session.rs` (`cmd_session_stop`, after Phase-1 `record_nudges_for_candidates`)
- Test: `mur-core/src/nudge/companion.rs` (inline)

> Discover agents at `~/.mur/agents/<slug>/`; read each `profile.yaml` and check `companion.enabled` (the `CompanionConfig` block on `AgentProfile`). Deliver each surfaced candidate to each enabled agent's `companion/inbox/`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn deliver_writes_to_enabled_agent_inboxes_only() {
    let mur = tempfile::tempdir().unwrap();
    // agent "on": profile with companion.enabled = true
    // agent "off": profile with companion.enabled = false
    // (write minimal profile.yaml for each under agents/<slug>/)
    let c = /* cand() */;
    let n = deliver_nudges_to_companions(mur.path(), &[c], "en").unwrap();
    assert_eq!(n, 1); // only the "on" agent got it
    assert!(mur.path().join("agents/on/companion/inbox/nudge_abc123.md").exists());
    assert!(!mur.path().join("agents/off/companion/inbox/nudge_abc123.md").exists());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::companion::tests::deliver_writes`
Expected: FAIL — `deliver_nudges_to_companions` undefined.

- [ ] **Step 3: Implement**

```rust
/// Write each candidate to every companion-enabled agent's inbox.
/// Returns the number of (agent × candidate) messages written.
pub fn deliver_nudges_to_companions(
    mur_dir: &Path,
    candidates: &[WorkflowCandidate],
    locale: &str,
) -> anyhow::Result<usize> {
    let agents_dir = mur_dir.join("agents");
    if !agents_dir.exists() || candidates.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    for entry in std::fs::read_dir(&agents_dir)? {
        let dir = entry?.path();
        let profile = dir.join("profile.yaml");
        if !profile.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&profile)?;
        let prof: mur_common::agent::AgentProfile = match serde_yaml_ng::from_str(&body) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !prof.companion.enabled {
            continue;
        }
        let inbox = dir.join("companion").join("inbox");
        std::fs::create_dir_all(&inbox)?;
        for c in candidates {
            write_nudge_inbox(&inbox, c, locale)?;
            written += 1;
        }
    }
    Ok(written)
}
```

(Confirm the `AgentProfile.companion.enabled` field path — grep `CompanionConfig` in `mur-common/src/agent.rs`. Match the real field name.)

In `mur-core/src/cmd/session.rs`, after the Phase-1 surfaced ids are computed:

```rust
let surfaced = record_nudges_for_candidates(&candidates)?;
if !surfaced.is_empty() && cfg.nudge.enabled {
    // re-load the surfaced candidates (Phase-1 stored snapshots in the ledger)
    let ledger = crate::nudge::NudgeLedger::load(&crate::nudge::NudgeLedger::default_path())?;
    let surfaced_cands: Vec<_> = surfaced.iter()
        .filter_map(|id| ledger.get(id).and_then(|r| r.candidate.clone()))
        .collect();
    let n = crate::nudge::companion::deliver_nudges_to_companions(
        &crate::default_mur_dir(), &surfaced_cands, &cfg.locale())?;
    eprintln!("💡 {} nudge(s) sent to your companion (or run `mur suggest`).", n.max(surfaced.len()));
}
```

(Use the real config locale accessor; if none, pass `"en"`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::companion::tests::deliver_writes`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/companion.rs mur-core/src/cmd/session.rs
git commit -m "feat(nudge): deliver nudges to companion-enabled agent inboxes"
```

---

## Task 5: `snooze` response value (mur-gui-core + mur-core)

**Files:**
- Modify: `mur-gui-core/src/companion_bridge/event.rs:43-75` (parser + `BridgeResponse`)
- Modify: `mur-core/src/cmd/agent_companion/inbox.rs` (`ack_at` signal validation, ~line 101)
- Test: `mur-gui-core/src/companion_bridge/event.rs` (inline)

> Today the validated response set is `good | bad | dismiss`. Add `snooze`. Keep it a plain signal value (the snooze *duration* comes from `config.nudge.snooze_days`, applied by the drain — Task 6).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn parses_snooze_response() {
    // build the inbox md text with ">>> response: snooze" and parse it
    let ev = parse_inbox_md_str(/* … md with response: snooze … */).unwrap();
    assert_eq!(ev.response, BridgeResponse::Signal("snooze".into()));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-gui-core parses_snooze_response`
Expected: FAIL — `snooze` rejected by the parser (`bail!("unrecognized response value")`).

- [ ] **Step 3: Implement**

In `event.rs`, extend the validated set:

```rust
} else if matches!(response_value, "good" | "bad" | "dismiss" | "snooze") {
    BridgeResponse::Signal(response_value.to_string())
}
```

In `mur-core/src/cmd/agent_companion/inbox.rs::ack_at`, extend the signal validation/mapping to accept `"snooze"` (it maps to a nudge decision in Task 6; for non-nudge messages, treat `snooze` as a no-op/dismiss-equivalent or reject — match the existing validation style and add `snooze` to the allowed set).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-gui-core parses_snooze_response`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-gui-core/src/companion_bridge/event.rs mur-core/src/cmd/agent_companion/inbox.rs
git commit -m "feat(nudge): add snooze response value"
```

---

## Task 6: Response drain → apply decision (mur-core)

**Files:**
- Modify: `mur-core/src/nudge/companion.rs` (the drain)
- Modify: `mur-core/src/cmd/workflow.rs` (`cmd_suggest` calls the drain first)
- Test: `mur-core/src/nudge/companion.rs` (inline) + `mur-core/tests/cli_nudge.rs`

> The drain scans companion inbox `.md` files (across agents) whose `id` is `nudge:*` and whose `>>> response:` is no longer `<unset>`, maps the response to a `NudgeDecision`, applies it via the Phase-1 emitter, and marks the inbox file consumed (rename/delete) so it is not re-applied.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn drain_applies_accept_and_creates_draft() {
    let mur = tempfile::tempdir().unwrap();
    // 1. seed nudge ledger (Phase 1) with a Surfaced candidate "abc123" (snapshot present)
    // 2. write a nudge inbox md for an agent with ">>> response: good"
    // 3. run the drain
    let applied = drain_nudge_responses_in(mur.path()).unwrap();
    assert_eq!(applied, 1);
    let l = crate::nudge::NudgeLedger::load(&mur.path().join("nudges.json")).unwrap();
    assert!(matches!(l.get("abc123").unwrap().state, crate::nudge::NudgeState::Accepted));
    // draft workflow created (workflow_store under this mur dir)
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-core nudge::companion::tests::drain_applies_accept`
Expected: FAIL — `drain_nudge_responses_in` undefined.

- [ ] **Step 3: Implement**

```rust
use crate::nudge::{NudgeDecision, NudgeEmitter, NudgeLedger};

fn signal_to_decision(sig: &str) -> Option<NudgeDecision> {
    match sig {
        "good" => Some(NudgeDecision::Accept),
        "dismiss" | "bad" => Some(NudgeDecision::Dismiss),
        "snooze" => Some(NudgeDecision::Snooze),
        _ => None,
    }
}

/// Scan all companion inboxes for answered nudge messages, apply each decision
/// to the nudge ledger, and consume the file. Returns count applied.
pub fn drain_nudge_responses_in(mur_dir: &Path) -> anyhow::Result<usize> {
    let cfg = mur_common::config::Config::load_or_default(&mur_common::config::Config::default_path());
    let ledger_path = NudgeLedger::default_path_in(mur_dir); // add a *_in(dir) variant for tests
    let mut ledger = NudgeLedger::load(&ledger_path)?;
    let now = chrono::Utc::now();
    let mut applied = 0;
    let agents = mur_dir.join("agents");
    if !agents.exists() { return Ok(0); }
    for agent in std::fs::read_dir(&agents)? {
        let inbox = agent?.path().join("companion").join("inbox");
        if !inbox.is_dir() { continue; }
        for f in std::fs::read_dir(&inbox)? {
            let path = f?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") { continue; }
            let text = std::fs::read_to_string(&path)?;
            let (id, resp) = parse_id_and_response(&text); // small local parser of frontmatter id + response line
            let (Some(id), Some(resp)) = (id, resp) else { continue };
            if resp == "<unset>" { continue; }
            let Some(cand_id) = candidate_id_from_msg(&id) else { continue };
            let Some(decision) = signal_to_decision(&resp) else { continue };
            NudgeEmitter::apply_decision(&mut ledger, &cand_id, decision, cfg.nudge.snooze_days, now,
                &|c| crate::cmd::workflow::create_draft_workflow(&c.suggested_name, &c.title, "", &c.evidence_session_ids))?;
            std::fs::remove_file(&path).ok(); // consume
            applied += 1;
        }
    }
    if applied > 0 { ledger.save(&ledger_path)?; }
    Ok(applied)
}
```

Implement `parse_id_and_response` (read the frontmatter `id:` and the `>>> response:` line). Add `NudgeLedger::default_path_in(dir)` (and have `default_path()` call it with `default_mur_dir()`) so tests can target a temp dir. Call `drain_nudge_responses_in(&default_mur_dir())` at the top of `cmd_suggest` (before listing pending), so accepted nudges turn into drafts whenever the user runs `mur suggest`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-core nudge::companion::tests::drain_applies_accept`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/nudge/companion.rs mur-core/src/cmd/workflow.rs
git commit -m "feat(nudge): drain companion responses → apply decision"
```

---

## Task 7: Enable by default (mur-common)

**Files:**
- Modify: `mur-common/src/config.rs` (`NudgeConfig::default` / `default` of `enabled`)
- Test: `mur-common/src/config.rs` (update Task-1 Phase-1 test)

- [ ] **Step 1: Update the test**

Change the Phase-1 `nudge_config_defaults` assertion:

```rust
assert!(c.enabled); // Phase 2: surface exists → on by default
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common nudge_config_defaults`
Expected: FAIL — still `false`.

- [ ] **Step 3: Flip the default**

In `NudgeConfig`, change the `enabled` default to `true` (update both the `#[serde(default = …)]`/field default and the `impl Default`). Replace the bare `#[serde(default)] pub enabled: bool` with an explicit `#[serde(default = "default_nudge_enabled")] pub enabled: bool` + `fn default_nudge_enabled() -> bool { true }`, and set `enabled: true` in `impl Default`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p mur-common nudge_config_defaults`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/config.rs
git commit -m "feat(nudge): enable nudges by default (Phase 2 surface live)"
```

---

## Task 8: GUI buttons (mur-hub-gui — TypeScript/React) [front-end]

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/companion.rs` (`companion_ack` — accept `snooze`)
- Modify: the React companion component (TS) that renders inbox messages.

> `mur-hub-gui` is a Tauri app (workspace-excluded), so this task is verified by build + manual smoke, not workspace `cargo test`.

- [ ] **Step 1: Accept `snooze` in the Tauri command**

In `mur-hub-gui/src-tauri/src/companion.rs::companion_ack`, extend the validation set to `{good, bad, dismiss, snooze}` (mirror Task 5).

- [ ] **Step 2: Render nudge actions in React**

In the companion message component, when `situation === "workflow_nudge"`, render three buttons:
- **Save it** → `invoke("companion_ack", { agent, msgId, signal: "good" })`
- **Not now** → `signal: "snooze"`
- **No thanks** → `signal: "dismiss"`

Gate the bubble's appearance on the existing DND state the GUI already tracks (do not pop a nudge while OS Focus/DND is active; it remains in the inbox and pops when DND clears).

- [ ] **Step 3: Build + smoke test**

Run the Tauri app build for `mur-hub-gui` (its own manifest, e.g. `npm run tauri build` / the project's documented GUI build). Manually: trigger a nudge (or hand-place a `workflow_nudge` inbox `.md`), confirm the bubble shows the three buttons, click **Save it**, then run `mur suggest` and confirm the draft workflow exists.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/src-tauri/src/companion.rs mur-hub-gui/src/<component>
git commit -m "feat(nudge): companion bubble Save/Not now/No thanks buttons"
```

---

## Task 9: End-to-end integration test (mur-core)

**Files:**
- Test: `mur-core/tests/cli_nudge.rs`

- [ ] **Step 1: Write the test**

```rust
#[test]
fn phase2_end_to_end_deliver_ack_drain() {
    let mur = tempfile::tempdir().unwrap();
    // 1. enable nudges; create a companion-enabled agent dir with profile.yaml.
    // 2. seed fingerprints → candidates; record + deliver to the agent inbox.
    // 3. simulate the GUI ack: rewrite the inbox file's ">>> response: <unset>" → "good".
    // 4. drain → draft created, ledger Accepted, inbox file consumed.
    // 5. re-deliver same candidate → no new message (ledger Accepted excludes it).
    // assert each step.
}
```

- [ ] **Step 2: Run + full suite + clippy/fmt**

Run:
```bash
cargo test -p mur-core nudge::
cargo test -p mur-core --test cli_nudge
cargo test -p mur-common
cargo test -p mur-gui-core companion_bridge
cargo clippy -p mur-core -p mur-common -p mur-gui-core -- -D warnings
cargo fmt --check
```
Expected: all PASS / clean.

- [ ] **Step 3: Commit**

```bash
git add mur-core/tests/cli_nudge.rs
git commit -m "test(nudge): phase 2 end-to-end deliver→ack→drain"
```

---

## Self-Review

**Spec coverage (§4 unit 4 + §6/§7 surface parts):**
- Proactive companion bubble → Tasks 3,4 (deliver) + 8 (render). Accept/dismiss/snooze → Tasks 5,6,8.
- DND respected → Task 8 (GUI display gate — the layer that owns `is_focus_active`).
- Accept → draft via existing path → Task 6 (drain calls Phase-1 `create_draft_workflow`).
- Anti-nag carried over from Phase 1 ledger (dismissed terminal, snooze window, daily cap) — Task 6 applies decisions through the same `NudgeEmitter`/ledger.
- Enable the surface → Task 7.

**Placeholder scan:** Code steps carry real code. "Match the exact frontmatter/`companion.enabled` field in `<file>`" notes point at concrete sources (`inbox.rs::write_inbox_md`, `ack_at`, `AgentProfile.companion`) that must be read, not invented. Task 8 is explicitly front-end (build+smoke, no Rust unit test) by nature.

**Type consistency:** `Situation::WorkflowNudge` (T1) used in T3 (frontmatter) + T8 (React switch). `nudge_msg_id`/`candidate_id_from_msg`/`nudge_body` (T2) used in T3,T6. `write_nudge_inbox` (T3) used in T4. `deliver_nudges_to_companions` (T4) used in session hook. `signal_to_decision`/`drain_nudge_responses_in` (T6) reuse Phase-1 `NudgeDecision`/`NudgeEmitter`/`create_draft_workflow`. `BridgeResponse::Signal("snooze")` (T5) consumed by T6's `signal_to_decision`.

**Cross-crate note:** Everything testable lands in `mur-core`/`mur-common`/`mur-gui-core` (workspace crates); only Task 8's UI is in the excluded `mur-hub-gui`. No `mur-agent-runtime` changes (delivery bypasses the outbox per Architecture decision 1).
