# MUR Pack S2 — Near-Term Shadow Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enforce the never-shadow principle from the Pack governance spec with a small, immediately-useful change: promote `mur-native-tools` into the shipped builtin skill set, and add a `mur skill doctor` check that detects agent-local skills shadowing a same-name global (builtin/registry) skill.

**Architecture:** Two independent code changes plus one operator rollout step. (1) `ensure_mur_skill` in `sync_cmd.rs` gains one more builtin entry so `mur-native-tools` ships with the binary and syncs to the global store `~/.mur/skills/`. (2) `skill_doctor.rs` gains a whole-store `shadow-drift` check that scans every agent's local skill dir and flags any skill whose name collides with a global-store skill — `Ok` severity when the vendored copy is byte-identical (redundant, safe to de-pin), `Warn` when it has diverged (a stale pinned snapshot). (3) After merge, the operator de-pins the live concierge's shadowing copies with the existing `mur agent skill remove`.

**Tech Stack:** Rust (edition 2024), `mur-common::skill` (`parse_canonical`, `content_hash_for_origin`), `mur-core` doctor/sync modules, `tempfile` for tests.

## Global Constraints

- No hardcoded values — reuse existing constants/helpers (`load_manifest`, `content_hash_for_origin`, `parse_canonical`). One line each:
  - Builtin skills are declared ONLY in `ensure_mur_skill`'s `skills: &[(&str, &str)]` array in `mur-core/src/cmd/sync_cmd.rs` using `include_str!("../skills/<snake_case>.yaml")`.
  - Builtin yaml files live in `mur-core/src/skills/` with `snake_case.yaml` filenames; the skill `name:` field is kebab-case.
  - Drift/equality comparison uses `mur_common::skill::content_hash_for_origin` (excludes the `origin`/`origin_version`/`origin_hash` stamp so restamping never counts as drift), NOT `content_hash_for_trust`.
  - Doctor `Severity` enum is exactly `{ Ok, Warn, Fail, Unknown }` — there is no `Info`; use `Ok` for the redundant-but-safe case.
  - Doctor operates on the GLOBAL store: `ctx.installed_skills` = `list_installed(~/.mur/skills)`. Agent-local skills live at `~/.mur/agents/<agent>/skills/<name>/skill.yaml`.
  - Brand name in any user-facing string is uppercase **MUR** (finding messages included).
- `concierge` (`~/.mur/agents/mur`) identity skills and `brainstorming` (`origin: registry:mur-official/brainstorming`) are NOT promoted to builtin and NOT removed. The check only flags agent-local skills whose name exists in the global store; `brainstorming` is not in the global store, so it is naturally not flagged.
- Run tests with the project's runner: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo test -p mur-core <test_name>` (the `mur-core` lib needs `MUR_WEB_DIST` to compile; `ORT_STRATEGY=download` avoids the onnxruntime link failure).

---

### Task 1: Promote `mur-native-tools` into the builtin skill set

**Files:**
- Create: `mur-core/src/skills/mur_native_tools.yaml`
- Modify: `mur-core/src/cmd/sync_cmd.rs` (the `skills` array in `ensure_mur_skill`, starts line 1072)
- Test: `mur-core/src/cmd/sync_cmd.rs` (a `#[cfg(test)] mod` test — add one if none exists in the file)

**Interfaces:**
- Consumes: `ensure_mur_skill(home: &std::path::Path, mur_root: &std::path::Path) -> Result<bool>` (existing, `pub(crate)`); writes each builtin to `mur_root/skills/<name>/skill.yaml`.
- Produces: `mur-native-tools` is now a global-store builtin after `mur sync`. Task 2's shadow-drift check relies on this: once synced, the concierge's agent-local `mur-native-tools` becomes a detectable shadow.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-core/src/cmd/sync_cmd.rs` (create the block at end of file if it does not exist, with `use super::*;`):

```rust
#[test]
fn ensure_mur_skill_ships_mur_native_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&root).unwrap();

    ensure_mur_skill(&home, &root).unwrap();

    let path = root.join("skills/mur-native-tools/skill.yaml");
    assert!(
        path.exists(),
        "mur-native-tools must be written to the global store by ensure_mur_skill"
    );
    let raw = std::fs::read_to_string(&path).unwrap();
    let m = mur_common::skill::parse_canonical(&raw).unwrap();
    assert_eq!(m.name, "mur-native-tools");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo test -p mur-core ensure_mur_skill_ships_mur_native_tools`
Expected: FAIL — the file `skills/mur-native-tools/skill.yaml` does not exist (assert fails).

- [ ] **Step 3: Create the builtin yaml**

Create `mur-core/src/skills/mur_native_tools.yaml` with EXACTLY this content (copied verbatim from the concierge's agent-local copy; universal MUR tool-selection knowledge, not concierge identity):

```yaml
name: mur-native-tools
version: 1.0.0
publisher: human:mur-official
description: 'How MUR should reach for its own tools when doing real work: prefer semantic project search over ad-hoc ls/grep for concept queries, check for an existing saved workflow before building from scratch, and offer quick replies as structured {text, description} options. Fires when the agent is about to search a codebase, look something up, plan a task, or offer the user choices.'
category: context
provenance: human
content:
  abstract: 'Tool-selection and interaction discipline for MUR when it acts as a working agent (not just the concierge greeter): use the right search tool for the query, reuse saved workflows, and format suggested replies the way the CLI renders them.'
  context: |
    ## Searching and looking things up

    - For a **concept / intent / "where is X handled"** query, call
      `mur_project_search` (semantic). Check `mur_project_status` first; if the
      project isn't indexed, say so instead of falling back to blind `ls`.
      *Why: semantic search finds code by meaning; `ls`/`grep` only find exact
      strings you already know, so starting with `ls -la` wastes a turn.*
    - Use **`grep`/`rg`** only for an **exact string or symbol** you already
      know (an error message, a function name).
      *Why: exact-match tools are precise but blind to synonyms — the opposite
      trade-off from semantic search.*
    - `mur_notes_search` is for the user's **saved notes/patterns**, not source
      code. Don't reach for it to answer a code question.
      *Why: it searches a different corpus and will return 0 results for code,
      which reads as "nothing found" when you simply asked the wrong index.*

    ## Reuse before you build

    - Before authoring a multi-step task from scratch, run `mur workflow list`
      (and `mur workflow show <name> --md`) to see if a saved workflow already
      covers it.
      *Why: MUR harvests reusable workflows; rebuilding one by hand throws away
      that memory and risks diverging from a known-good sequence.*

    ## Suggested replies (the chooser)

    - When you offer the user a small set of choices, call `suggest_replies`
      with structured options: each `{ text, description }` — `text` is the
      message they'd send, `description` is a one-line trade-off.
      *Why: the CLI renders the description under each option; a bare string
      leaves the user picking blind.*
    - Do **not** number the options yourself ("A:", "1.") — the UI marks and
      spaces them.
      *Why: hand-numbering double-labels every row and fights the UI's own
      selection caret.*
    - Use `suggest_replies` when you genuinely need the user to choose before
      you can continue — then end your turn and wait for their pick. But when
      you already know the next step (they've approved a plan, the path is
      clear), just take it — delegate the approved design to the `pm` agent,
      run the workflow, write the file. Prefer acting over asking; don't offer
      a chooser in place of an action, and after an approval act instead of
      re-asking.
      *Why: the chooser is for real decisions, not a reflex. If every step ends
      in another chooser, the work never happens (e.g. the pm hand-off is
      dropped). Choose the path that moves the task forward.*

    Sources: MUR MCP tool set (mur_project_search / mur_notes_search /
    mur_project_status); suggest_replies structured-option schema.
tags:
- tooling
- search
- workflow
- interaction
triggers:
- type: keyword
  pattern: search|find|look up|where is|workflow|建立|尋找|查詢|搜尋|工作流
- type: session_start
- type: manual
priority: normal
updated_at: 1970-01-01T00:00:00Z
```

- [ ] **Step 4: Register it in `ensure_mur_skill`**

In `mur-core/src/cmd/sync_cmd.rs`, add this entry to the `skills` array (place it next to the other `mur-*` context skills, e.g. right after the `("mur-context", …)` line):

```rust
        (
            "mur-native-tools",
            include_str!("../skills/mur_native_tools.yaml"),
        ),
```

- [ ] **Step 5: Run test to verify it passes**

Run: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo test -p mur-core ensure_mur_skill_ships_mur_native_tools`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/skills/mur_native_tools.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): promote mur-native-tools to the builtin skill set"
```

---

### Task 2: Add the `shadow-drift` check to `mur skill doctor`

**Files:**
- Modify: `mur-core/src/cmd/skill_doctor.rs` (the `all_checks` array ~line 150; the check dispatch after the per-skill loop ~line 212; add `run_shadow_drift`)
- Test: `mur-core/src/cmd/skill_doctor.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `DoctorCtx { home: PathBuf, installed_skills: HashSet<String>, … }`; `load_manifest(home: &Path, skill_name: &str) -> Option<SkillManifest>`; `mur_common::skill::{parse_canonical, content_hash_for_origin}`; `Finding { check_id, category, severity, skill_name, message, remediation, fixable }`; `Severity::{Ok, Warn}`.
- Produces: `run_shadow_drift(ctx: &DoctorCtx) -> Vec<Finding>`. Runs once per `doctor` invocation (whole-store scan, not per-skill). `check_id = "shadow-drift"`, `category = "shadow"`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `mur-core/src/cmd/skill_doctor.rs`. These build a temp home with one global skill and an agent that vendors a same-name skill. Helper to write a minimal skill:

```rust
fn write_skill(dir: &std::path::Path, name: &str, abstract_text: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let yaml = format!(
        "name: {name}\nversion: 1.0.0\ndescription: 'test skill {name}'\ncategory: context\ncontent:\n  abstract: '{abstract_text}'\n  context: 'body'\n"
    );
    std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
}

#[test]
fn shadow_drift_flags_diverged_agent_copy_as_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    write_skill(&home.join("skills/foo"), "foo", "global version");
    write_skill(&home.join("agents/a1/skills/foo"), "foo", "agent-diverged version");

    let mut ctx = doctor_ctx(&tmp);
    ctx.installed_skills.insert("foo".to_string());

    let findings = run_shadow_drift(&ctx);
    let f = findings.iter().find(|f| f.skill_name == "foo").expect("expected a shadow finding for foo");
    assert_eq!(f.check_id, "shadow-drift");
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.remediation.as_deref().unwrap().contains("mur agent skill remove a1 skills/foo"));
}

#[test]
fn shadow_drift_flags_identical_agent_copy_as_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    write_skill(&home.join("skills/foo"), "foo", "same version");
    write_skill(&home.join("agents/a1/skills/foo"), "foo", "same version");

    let mut ctx = doctor_ctx(&tmp);
    ctx.installed_skills.insert("foo".to_string());

    let findings = run_shadow_drift(&ctx);
    let f = findings.iter().find(|f| f.skill_name == "foo").expect("expected a shadow finding for foo");
    assert_eq!(f.severity, Severity::Ok);
}

#[test]
fn shadow_drift_ignores_agent_only_skill() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    // agent-local skill with NO global twin — legitimate, must not be flagged
    write_skill(&home.join("agents/a1/skills/private"), "private", "agent only");

    let ctx = doctor_ctx(&tmp); // installed_skills empty
    let findings = run_shadow_drift(&ctx);
    assert!(findings.is_empty(), "agent-only skills must not be flagged as shadows");
}
```

Note: `doctor_ctx(&TempDir)` is the existing test helper (line ~1270) — it sets `home = dir.path()`. The tests mutate `installed_skills` on the returned ctx (make the binding `mut`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo test -p mur-core shadow_drift`
Expected: FAIL — `run_shadow_drift` is not defined (compile error).

- [ ] **Step 3: Implement `run_shadow_drift`**

Add this function to `mur-core/src/cmd/skill_doctor.rs` (near the other `run_*` check functions):

```rust
/// Whole-store scan: detect agent-local skills that shadow a same-name skill
/// in the global store (builtin or registry). A vendored copy identical to
/// the global one is redundant (Ok — safe to de-pin); a diverged copy is a
/// shadow that silently pins a stale snapshot and never receives upstream
/// updates (Warn). Only names present in the global store can shadow, so
/// genuinely agent-specific skills are never flagged.
fn run_shadow_drift(ctx: &DoctorCtx) -> Vec<Finding> {
    let mut findings = Vec::new();
    let Ok(agents) = std::fs::read_dir(ctx.home.join("agents")) else {
        return findings;
    };
    for agent in agents.flatten() {
        if !agent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let agent_name = agent.file_name().to_string_lossy().into_owned();
        let Ok(skills) = std::fs::read_dir(agent.path().join("skills")) else {
            continue;
        };
        for skill in skills.flatten() {
            let name = skill.file_name().to_string_lossy().into_owned();
            if !ctx.installed_skills.contains(&name) {
                continue; // no global twin → legitimately agent-specific
            }
            let Ok(local_raw) = std::fs::read_to_string(skill.path().join("skill.yaml")) else {
                continue;
            };
            let (Ok(local), Some(global)) = (
                mur_common::skill::parse_canonical(&local_raw),
                load_manifest(&ctx.home, &name),
            ) else {
                continue;
            };
            let (Ok(local_hash), Ok(global_hash)) = (
                mur_common::skill::content_hash_for_origin(&local),
                mur_common::skill::content_hash_for_origin(&global),
            ) else {
                continue;
            };
            let remediation = Some(format!("mur agent skill remove {agent_name} skills/{name}"));
            if local_hash == global_hash {
                findings.push(Finding {
                    check_id: "shadow-drift".into(),
                    category: "shadow".into(),
                    severity: Severity::Ok,
                    skill_name: name.clone(),
                    message: format!(
                        "Agent '{agent_name}' vendors '{name}', identical to the global copy — redundant. De-pin so the global (builtin/registry) copy owns it."
                    ),
                    remediation,
                    fixable: false,
                });
            } else {
                findings.push(Finding {
                    check_id: "shadow-drift".into(),
                    category: "shadow".into(),
                    severity: Severity::Warn,
                    skill_name: name.clone(),
                    message: format!(
                        "Agent '{agent_name}' vendors '{name}', diverged from the global copy — this shadow pins a stale snapshot and never receives upstream MUR updates."
                    ),
                    remediation,
                    fixable: false,
                });
            }
        }
    }
    findings
}
```

- [ ] **Step 4: Register the check id and wire the call**

In `mur-core/src/cmd/skill_doctor.rs`, add `"shadow-drift"` to the `all_checks` array (the array literal ending `"disclosure",` at line ~151):

```rust
        "disclosure",
        "shadow-drift",
    ];
```

Then, immediately AFTER the per-skill loop closes (the `}` at line ~212, before the `// ── Repair (M5b) ──` block), add the whole-store dispatch:

```rust
    if active_checks.contains(&"shadow-drift") {
        findings.extend(run_shadow_drift(&ctx));
    }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo test -p mur-core shadow_drift`
Expected: PASS (all three tests).

- [ ] **Step 6: Verify no clippy regressions on the touched files**

Run: `MUR_WEB_DIST=$HOME/Projects/mur-web/dist ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: no warnings. (Watch for `clippy::cmp_owned` — the code compares `String` hashes with `==`, which is fine; the `let (Ok(..), Some(..)) = (..) else` tuple pattern is stable in edition 2024.)

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/skill_doctor.rs
git commit -m "feat(doctor): add shadow-drift check for agent-local skills shadowing the global store"
```

---

## Rollout / Operator steps (post-merge, not code)

These mutate live `~/.mur` data and need the operator's consent — they are NOT part of the TDD tasks. Run them after the PR merges and a fresh `mur` is installed (so `mur sync` writes `mur-native-tools` into `~/.mur/skills/`).

1. Sync the new builtin into the global store: `mur sync` (or whatever the release install runs).
2. Confirm the check surfaces the concierge's shadows: `mur skill doctor` — expect `shadow-drift` findings for `mur-native-tools`, `mur-compress`, `parallel-code`, `video-analyze`, `watch-together` on agent `mur`.
3. De-pin each shadow using the existing command (removes the agent-local copy + its profile entry; the global builtin then owns the skill and injects store-wide):
   ```bash
   mur agent skill remove mur skills/mur-native-tools
   mur agent skill remove mur skills/mur-compress
   mur agent skill remove mur skills/parallel-code
   mur agent skill remove mur skills/video-analyze
   mur agent skill remove mur skills/watch-together
   ```
4. Leave `concierge` identity skills and `brainstorming` (registry-owned) in place. Re-run `mur skill doctor` to confirm the shadow findings are gone.

---

## Self-Review

**Spec coverage (spec §8 item 1 — S2):**
- "promote `mur-native-tools` into the builtin set (`sync_cmd.rs`)" → Task 1. ✅
- "add a `mur skill doctor` shadow-drift check (local copy's content differs from a same-name builtin/registry copy → warn)" → Task 2. ✅
- "de-pin the 4 concierge skills that shadow builtin" → Rollout step 3 (existing `mur agent skill remove`, no new code needed). ✅ (plus `mur-native-tools` itself, which becomes a shadow after Task 1)
- "Keep `concierge` (identity) and `brainstorming` (registry-owned, stays registry)" → Global Constraints + Rollout step 4; the check's `installed_skills.contains` gate naturally excludes `brainstorming`. ✅

**Placeholder scan:** No TBD/TODO. All code blocks are complete and compile-ready; the yaml is verbatim.

**Type consistency:** `Severity::{Ok, Warn}` match the confirmed enum. `Finding` fields match the struct (`check_id, category, severity, skill_name, message, remediation, fixable`). `content_hash_for_origin` / `parse_canonical` / `load_manifest` signatures verified against source. `run_shadow_drift(&ctx)` is called once after the per-skill loop, gated on `active_checks`.

**Scope:** Two focused code tasks + one operator rollout. Reference-with-embed-fallback resolution, the unified pack kernel, and a `--fix` executor are deliberately OUT of S2 (they are S1/S3 per the spec). `ponytail:` no `--fix` auto-remove — the finding carries the exact remediation command; add an executor when multiple fixable check types justify it.
