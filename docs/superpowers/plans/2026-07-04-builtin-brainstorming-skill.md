# Built-in Brainstorming Skill Implementation Plan (2/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every fresh Hub install seeds the default concierge with an official, origin-stamped brainstorming skill; existing installs gain it on next Hub upgrade without touching anything they already have.

**Architecture:** Port `superpowers:brainstorming` to a MUR skill (derived work, MIT attribution), add it to the bundled concierge template (`mur-hub-gui/src-tauri/resources/mur-agent-template/`), stamp it with `origin: registry:mur-official/brainstorming` so Plan 1's pipeline upgrades it, and extend the seeder with a seed-missing-skills pass that never overwrites. Spec §A. **Depends on Plan 1 (origin fields).**

**Tech Stack:** Rust, MUR skill YAML schema (`mur-common/src/skill`), existing `seed_mur.rs` seeder + its regression tests.

## Global Constraints

- Same as Plan 1 (nextest, ORT_STRATEGY, fmt+clippy, ≤800 lines/file).
- Seeder invariant: **seed only what is absent; never overwrite** — upgrades belong to the registry pipeline.
- Brand copy uses uppercase "MUR"; skill `name` stays lowercase (`brainstorming`).

---

### Task 1: Author the brainstorming skill

**Files:**
- Create: `mur-hub-gui/src-tauri/resources/mur-agent-template/skills/brainstorming/skill.yaml`
- Modify: `mur-hub-gui/src-tauri/resources/mur-agent-template/profile.yaml` (append `- skills/brainstorming` to `skills:`)

**Interfaces:**
- Produces: a valid `SkillManifest` (passes `mur_common::skill::validate`) named `brainstorming`, `publisher: mur-official`, `version: 1.0.0`, category matching an existing `Category` variant (use the same one the concierge skill uses; check `skills/concierge/skill.yaml`), with `origin: registry:mur-official/brainstorming`, `origin_version: 1.0.0`, `origin_hash` computed via `content_hash_for_origin` (compute with a tiny one-off: `cargo run -p mur-core --example` is overkill — add a `#[test]` in Task 2 that recomputes and asserts the stamp, and paste the value it prints on first failure).

- [ ] **Step 1: Write the content.** Port from `/Users/david/.claude/plugins/cache/claude-plugins-official/superpowers/6.1.1/skills/brainstorming/SKILL.md`, adapted to the MUR fleet context. Required adaptations (the port is a rewrite, not a copy):
  - Dialogue partner is the human via `murmur`; one question at a time; multiple-choice preferred; 2–3 approaches with a recommendation; sectioned design presentation with per-section confirmation.
  - Terminal state: hand the agreed design summary to the `pm` agent (via fleet channel/delegation) for spec/plan authoring — NOT Claude Code's writing-plans skill, NOT any implementation action.
  - Remove everything harness-specific: subagent dispatch, plan mode, Skill tool, visual companion, `docs/superpowers/specs` paths.
  - Triggers: "brainstorm", "腦暴", "頭腦風暴", "design discussion", "設計討論", "幫我想".
  - Footer note in the yaml `description` or content: "Derived from obra/superpowers brainstorming (MIT)."
- [ ] **Step 2: Validate.** `cargo nextest run -p mur-hub-gui bundled_template` — the existing loader regression test (`seed_mur.rs:377` area) must still pass; it validates template skills load. If it only checks `concierge`, that's fine — Task 2 adds the sibling test.
- [ ] **Step 3: Commit** — `git add mur-hub-gui/src-tauri/resources/mur-agent-template && git commit -m "feat(hub): bundled brainstorming skill for default concierge"`

### Task 2: Template regression test for the new skill

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/seed_mur.rs` (test module, next to `bundled_template_concierge_skill_is_loadable`)

- [ ] **Step 1: Write the test** (mirror the concierge test at `seed_mur.rs:377`):

```rust
#[test]
fn bundled_template_brainstorming_skill_is_loadable_and_stamped() {
    use mur_common::skill::{read_from_dir, validate, hash::content_hash_for_origin};
    let tpl = /* same template-dir resolution the concierge test uses */;
    assert!(std::fs::read_to_string(tpl.join("profile.yaml")).unwrap()
        .contains("- skills/brainstorming"));
    let skill = read_from_dir(&tpl.join("skills/brainstorming")).expect("loads");
    validate(&skill).expect("validates");
    assert_eq!(skill.origin.as_deref(), Some("registry:mur-official/brainstorming"));
    assert_eq!(skill.origin_version.as_deref(), skill.version.as_str().into());
    assert_eq!(skill.origin_hash.as_deref().unwrap(),
        content_hash_for_origin(&skill).unwrap());
}
```

- [ ] **Step 2: Run** — first run fails if the pasted `origin_hash` is wrong; the assertion message shows the computed value — fix the yaml, re-run to PASS. (`cargo nextest run -p mur-hub-gui brainstorming` — needs the Hub test env; if the hub crate won't build workspace-side, run via its own manifest: `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml brainstorming`.)
- [ ] **Step 3: Commit** — `git commit -am "test(hub): template brainstorming skill regression"`

### Task 3: Seed-missing-skills pass for existing installs

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/seed_mur.rs` (`seed_mur_if_missing`, ~line 276, plus its caller)
- Test: same file's test module

**Interfaces:**
- Produces: `pub fn seed_missing_template_skills(template_dir: &Path, mur_home: &Path) -> std::io::Result<Vec<String>>` — for each `template_dir/skills/<name>`, if `<mur_home>/agents/mur/skills/<name>` does NOT exist, copy the dir and append `- skills/<name>` to the agent's `profile.yaml` skills list (only if not already referenced). Existing dirs are untouched byte-for-byte. Returns the seeded names. Called on every Hub launch right after the existing seed check (cheap no-op when nothing is missing).

- [ ] **Step 1: Write the failing tests:**
  - `seeds_missing_skill_into_existing_agent` — agent exists with only `concierge`; run; `brainstorming` dir appears, profile gains the reference, returns `["brainstorming"]`.
  - `never_overwrites_existing_skill` — pre-create `skills/brainstorming/skill.yaml` with sentinel content `name: brainstorming\n# USER EDIT`; run; file byte-identical, returns `[]`.
  - `idempotent_profile_reference` — run twice; profile contains the reference exactly once.
- [ ] **Step 2: Run to verify they fail.**
- [ ] **Step 3: Implement** (reuse the file-copy helper `copy_tree` already in `seed_mur.rs`; profile edit = read yaml, parse skills list, append, atomic write same as the seeder's existing profile handling).
- [ ] **Step 4: Run tests** — PASS; clippy clean.
- [ ] **Step 5: Commit** — `git commit -am "feat(hub): seed missing bundled skills on upgrade (never overwrite)"`

### Task 4: Publish to the official registry

**Files (external repo):** `https://github.com/mur-run/skill-registry` — add `skills/brainstorming/1.0.0/skill.yaml` (identical content, WITHOUT the origin stamp — the stamp is applied at install; registry copies are canonical unstamped) and an `index.yaml` entry (`latest: 1.0.0`, `publisher: mur-official`, `content_sha256` per the registry's existing convention).

- [ ] **Step 1:** Clone registry repo, add files following an existing skill's layout exactly.
- [ ] **Step 2:** Verify locally: `mur skill upgrade --check` on a machine with the template-seeded skill reports `UpToDate` (stamp version == registry latest).
- [ ] **Step 3:** Commit + push to the registry repo (PR if it has branch protection).
