# Agent Output Locations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every file a MUR agent produces lands where MUR can read it (`~/.mur`), never dumped into the working directory / a source tree.

**Architecture:** Inject a constant "Output locations" guidance block into every agent's system prompt from the runtime (`assemble_system_prompt`), so it ships in the binary and reaches every agent. Reinforce it in the `mur-workflow-author` skill. Clean up the one stray artifact this bug already produced.

**Tech Stack:** Rust (`mur-agent-runtime`), YAML skill manifests, `mur` CLI.

## Global Constraints

- No enforcement — fs entitlements and `working_dir` are unchanged; the rule is injected text, not a filesystem guardrail. (spec Non-Goals)
- No new CLI commands — reuse `mur skill install` / `mur workflow new`. (spec Non-Goals)
- Run-artifact path is exactly `~/.mur/artifacts/<agent-name>/<run>/`. (spec)
- Brand name in user-facing copy is uppercase **MUR**. (CLAUDE.md rule 7)
- `cargo fmt` + `cargo clippy -D warnings` must pass; CI compiles with stable rustfmt. (CLAUDE.md)

Spec: `docs/superpowers/specs/2026-07-07-agent-output-locations-design.md`

---

### Task 1: Inject the "Output locations" rule in the runtime

**Files:**
- Modify: `mur-agent-runtime/src/task_runner.rs` (add `const`; append to `base` inside `assemble_system_prompt`, ~line 471; add test in `mod tests`, ~line 1852)

**Interfaces:**
- Consumes: `TaskRunner::new_stub_echo()`, `.with_system_prompt(Option<String>)`, `TaskRunner::assemble_system_prompt(&self, user_prompt: &str, active_fleet: Option<&str>, active_team: Option<&str>) -> (String, Vec<String>)` (all already exist).
- Produces: `const OUTPUT_LOCATIONS_RULE: &str` folded into the returned system prompt on every turn.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `mur-agent-runtime/src/task_runner.rs`:

```rust
#[test]
fn assemble_system_prompt_appends_output_locations_rule() {
    let runner = TaskRunner::new_stub_echo().with_system_prompt(Some("BASE PROMPT".into()));
    let (sys, _fired) = runner.assemble_system_prompt("hello", None, None);
    assert!(sys.starts_with("BASE PROMPT"), "keeps the agent's own prompt first");
    assert!(sys.contains("Output locations"), "injects the rule heading");
    assert!(sys.contains("~/.mur/artifacts/"), "names the run-artifact dir");
    assert!(sys.contains("mur skill install"), "names the register command");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-agent-runtime assemble_system_prompt_appends_output_locations_rule`
Expected: FAIL — `sys` lacks "Output locations" (rule not injected yet).

- [ ] **Step 3: Add the constant**

Insert near the top of `mur-agent-runtime/src/task_runner.rs` (module-level, above `impl TaskRunner` — beside other consts):

```rust
/// Injected into every agent's system prompt so authored files land where MUR
/// can read them instead of the working directory. Guidance, not enforcement.
const OUTPUT_LOCATIONS_RULE: &str = "\n\n## Output locations\n\
When you produce files, put them where MUR can read them — never write them into the current working directory (often a source tree):\n\
- Knowledge objects (workflows, skills, notes): register with the real command so they land in ~/.mur and show up in MUR and the Hub — `mur skill install <path>` for a skill, `mur workflow new` for a workflow. Never leave the definition in the working directory.\n\
- Run artifacts (reports, quarantined files, scratch output): write to ~/.mur/artifacts/<your-agent-name>/<run>/, where <run> is a short timestamp or task label. Never the working directory.\n\
- The only reason to write into the working directory is to edit an existing file in a repository you have been granted access to.";
```

- [ ] **Step 4: Append the constant to `base`**

In `assemble_system_prompt`, change the first line from:

```rust
        let base = self.system_prompt.clone().unwrap_or_default();
```

to:

```rust
        let mut base = self.system_prompt.clone().unwrap_or_default();
        base.push_str(OUTPUT_LOCATIONS_RULE);
```

This covers both return paths — the early `return (base, vec![])` when the agent has no skills, and the `combined` path below.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p mur-agent-runtime assemble_system_prompt_appends_output_locations_rule`
Expected: PASS.

- [ ] **Step 6: Full check + fmt + clippy**

Run: `cargo test -p mur-agent-runtime && cargo fmt -p mur-agent-runtime --check && cargo clippy -p mur-agent-runtime -- -D warnings`
Expected: all pass. (Env: `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH" ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist`)

- [ ] **Step 7: Commit**

```bash
git add mur-agent-runtime/src/task_runner.rs
git commit -m "feat(agent-runtime): inject output-location rule into every agent prompt"
```

---

### Task 2: Reinforce the rule in the `mur-workflow-author` skill

**Files:**
- Modify: `~/.mur/skills/mur-workflow-author/skill.yaml` (local install; not repo-tracked — this is a runtime data file)

**Interfaces:**
- Consumes: nothing. Produces: nothing code-facing. This is a content edit so the authoring flow itself states the destination rule.

- [ ] **Step 1: Read the current skill body**

Run: `mur skill show mur-workflow-author --md`
Expected: prints the manifest; note where `content.context` (the body rules) ends.

- [ ] **Step 2: Add a destination rule to the skill body**

Edit `~/.mur/skills/mur-workflow-author/skill.yaml`, appending one rule to `content.context` (match the existing imperative "rule + why" style):

```
- Write the authored workflow/skill into ~/.mur (via `mur workflow new` or
  `mur skill install <path>`), never into the current working directory.
  *Why: only files under ~/.mur are visible to MUR retrieval and the Hub; a
  definition left in a source tree is invisible and pollutes the checkout.*
```

- [ ] **Step 3: Validate the edited skill**

Run: `mur skill validate ~/.mur/skills/mur-workflow-author/skill.yaml`
Expected: prints `ok:` (schema + security scan pass).

- [ ] **Step 4: Reindex so retrieval picks up the new body**

Run: `mur skill reindex-vec`
Expected: completes without error.

No commit — this file lives under `~/.mur`, outside the repo.

---

### Task 3: Install the stray security-scan skill and confirm it is visible

**Files:**
- Source: `registry-work/skills/remote-security-scan/1.0.0/skill.yaml` (now gitignored)
- Result: `~/.mur/skills/remote-security-scan/skill.yaml` (created by install)

**Interfaces:**
- Consumes: `mur skill install <path>`. Produces: an installed, registered skill.

- [ ] **Step 1: Validate the draft before installing**

Run: `mur skill validate registry-work/skills/remote-security-scan/1.0.0/skill.yaml`
Expected: `ok:`. If it fails, fix the manifest per the validator message before continuing.

- [ ] **Step 2: Install it**

Run: `mur skill install registry-work/skills/remote-security-scan/1.0.0/skill.yaml`
Expected: reports the skill installed into `~/.mur/skills/`.

- [ ] **Step 3: Confirm MUR + Hub can see it**

Run: `mur skill list | grep remote-security-scan`
Expected: the skill appears in the list. (In the Hub, a refresh shows it under skills.)

No repo commit — installs land under `~/.mur`.

---

## Self-Review

**Spec coverage:**
- "Inject the rule once in the runtime" → Task 1.
- "Reinforce in mur-workflow-author skill" → Task 2.
- "Install the stray draft" → Task 3.
- "gitignore registry-work/" → already done in commit `27c89cb4` (with the spec).
- "Unit test on assemble_system_prompt" → Task 1 Step 1.
- Verification points 1–3 (behavioral) are covered by Task 3 Step 3 plus manual murmur re-run — left as operator verification, noted in the spec.

**Placeholders:** none — const text, test code, and exact commands are all inline.

**Type consistency:** `assemble_system_prompt` signature and `with_system_prompt(Option<String>)` / `new_stub_echo()` match the existing code read during grounding.

**Note:** Tasks 2 and 3 mutate `~/.mur` (runtime data), so only Task 1 produces a repo commit. The branch/PR for this plan is Task 1 plus the already-committed spec + gitignore.
