# MUR Pack S1 — De-Shadow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give MUR a working way to remove agent-local vendored copies of builtin skills that shadow the global store, and an automatic safe cleanup — CLI-only.

**Architecture:** Three cohesive changes. (1) Extract a `depin_skill(home, agent, skill)` core in `cmd/agent/skill.rs` that removes a skill from all three places it can live (`skills:` refs, `installed_skills:` cards, on-disk dir); `cmd_skill_remove` becomes a thin wrapper. (2) Fix the S2 `shadow-drift` finding's remediation string and set its `fixable` flag (identical shadows fixable, diverged not). (3) Add a `ShadowDepinRepair` to the existing `mur skill doctor --fix` repair framework that calls `depin_skill` for the identical shadows only.

**Tech Stack:** Rust (edition 2024), `mur-common::AgentProfile`/`SkillCardEntry`, `mur-core` skill/doctor/skill_repair modules, `serde_yaml_ng`, `tempfile` for tests.

## Global Constraints

- Reuse existing helpers; no hardcoded values:
  - Profile load/save: construct path `home/agents/<agent>/profile.yaml`, parse with `serde_yaml_ng::from_str::<_AgentProfile>`, save with the existing `save_profile(&path, &mut profile)` (its #717 guard only rejects *newly added* dangling refs — de-pin removes refs, so it never fires).
  - `_AgentProfile` = `mur_common::AgentProfile`; fields used: `skills: Vec<String>`, `installed_skills: Vec<SkillCardEntry>`. `SkillCardEntry.name: String`.
  - `resolve_skill_id(&profile, query) -> Option<&String>` (in `cmd/agent/skill.rs`) matches a query against `skills:` refs in three forms (full id / basename / stem). Reuse it for the ref-removal step.
  - Repair framework (`mur-core/src/skill_repair/`): `trait Repair { fn check_id(&self)->&'static str; fn applicable(&self, &Finding)->bool; fn run(&self, &Finding, &RepairCtx, apply: bool)->RepairOutcome; }`; `RepairCtx { home: &Path, registry_url: &str }`; `RepairOutcome::{Fixed, DryRun(String), Skipped(String), Failed(String)}`. `run_repairs` only dispatches findings where `finding.fixable == true`.
  - `Finding` has NO structured agent field. The repair recovers `(agent, skill)` by parsing the remediation string it controls (`mur agent skill remove <agent> <skill>`). This is an internal contract (generator and parser both in this codebase).
  - `mur skill doctor --fix` is a dry-run; `mur skill doctor --fix --apply` actually applies (behind an interactive confirm). Destructive de-pin therefore only runs under `--apply`.
- Brand name "MUR" uppercase in any user-facing string.
- Run tests with: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUSTFLAGS=-Cdebuginfo=0 CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target cargo test -p mur-core <name>` (add `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` to PATH if `cargo` is missing). A cold compile takes several minutes.

---

### Task 1: `depin_skill` core + rewire `cmd_skill_remove`

**Files:**
- Modify: `mur-core/src/cmd/agent/skill.rs` (add `depin_skill`; rewrite `cmd_skill_remove` at ~line 218)
- Test: `mur-core/src/cmd/agent/skill.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `resolve_skill_id`, `save_profile`, `resolve_mur_home`, `_AgentProfile`, `SkillCardEntry` (all already imported in the file).
- Produces: `pub(crate) fn depin_skill(home: &Path, agent: &str, skill: &str) -> Result<bool>` — removes the skill from `skills:` refs, `installed_skills:` cards, and the on-disk dir under `home`; returns `true` if it was present in any location. Task 3's repair calls this with `ctx.home`.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `mur-core/src/cmd/agent/skill.rs`:

```rust
fn write_profile(home: &std::path::Path, agent: &str, body: &str) -> std::path::PathBuf {
    let dir = home.join("agents").join(agent);
    std::fs::create_dir_all(dir.join("skills")).unwrap();
    let path = dir.join("profile.yaml");
    std::fs::write(&path, body).unwrap();
    path
}

fn write_agent_skill_dir(home: &std::path::Path, agent: &str, name: &str) {
    let d = home.join("agents").join(agent).join("skills").join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("skill.yaml"),
        format!("name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: b\n"),
    )
    .unwrap();
}

// A minimal profile carrying `name` as an installed_skills card + on-disk dir,
// but NOT as a skills: ref (the exact concierge shadow shape).
const CARD_ONLY_PROFILE: &str = "\
name: mur
display_name: MUR
model_ref: m
skills:
  - skills/concierge
installed_skills:
- name: mur-compress
  version: 1.0.0
  publisher: human:mur
  description: d
  category: context
  abstract: a
";

#[test]
fn depin_removes_card_and_dir_when_not_a_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_profile(home, "mur", CARD_ONLY_PROFILE);
    write_agent_skill_dir(home, "mur", "mur-compress");
    write_agent_skill_dir(home, "mur", "concierge");

    let removed = depin_skill(home, "mur", "mur-compress").unwrap();
    assert!(removed, "should report removal");

    // card gone
    let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
    let p: _AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert!(p.installed_skills.iter().all(|c| c.name != "mur-compress"));
    // dir gone
    assert!(!home.join("agents/mur/skills/mur-compress").exists());
    // untouched skill dir preserved
    assert!(home.join("agents/mur/skills/concierge").exists());
}

#[test]
fn depin_returns_false_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_profile(home, "mur", CARD_ONLY_PROFILE);
    let removed = depin_skill(home, "mur", "does-not-exist").unwrap();
    assert!(!removed);
}

#[test]
fn depin_removes_ref_form_too() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // concierge is a skills: ref + on-disk dir
    write_profile(home, "mur", CARD_ONLY_PROFILE);
    write_agent_skill_dir(home, "mur", "concierge");
    let removed = depin_skill(home, "mur", "concierge").unwrap();
    assert!(removed);
    let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
    let p: _AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
    assert!(!p.skills.iter().any(|s| s == "skills/concierge"));
    assert!(!home.join("agents/mur/skills/concierge").exists());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `… cargo test -p mur-core depin_`
Expected: FAIL — `depin_skill` not defined (compile error).

- [ ] **Step 3: Implement `depin_skill` and rewire `cmd_skill_remove`**

In `mur-core/src/cmd/agent/skill.rs`, add `depin_skill` (near `cmd_skill_remove`) and replace the body of `cmd_skill_remove`:

```rust
/// Remove a skill from every place it can live on an agent: the `skills:`
/// reference list, the `installed_skills:` card list, and the on-disk
/// `skills/<name>/` dir under `home`. Returns whether it was present anywhere.
/// Takes an explicit `home` so the doctor repair can operate on any store.
pub(crate) fn depin_skill(home: &Path, agent: &str, skill: &str) -> Result<bool> {
    let path = home.join("agents").join(agent).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{agent}' not found");
    }
    let yaml = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut profile: _AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;

    // Bare skill name, accepting `skills/foo`, `foo.yaml`, or `foo`.
    let base = Path::new(skill)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(skill)
        .to_string();

    let mut removed = false;

    // 1. skills: ref (any resolvable form)
    if let Some(resolved) = resolve_skill_id(&profile, skill).cloned() {
        profile.skills.retain(|s| s != &resolved);
        removed = true;
    }
    // 2. installed_skills: card by name
    let before = profile.installed_skills.len();
    profile.installed_skills.retain(|c| c.name != base);
    if profile.installed_skills.len() != before {
        removed = true;
    }
    if removed {
        save_profile(&path, &mut profile)?;
    }
    // 3. on-disk vendored dir (only if no surviving ref still points at it)
    let dir = home.join("agents").join(agent).join("skills").join(&base);
    let still_referenced = profile
        .skills
        .iter()
        .any(|s| Path::new(s).file_stem().and_then(|f| f.to_str()) == Some(base.as_str()));
    if dir.is_dir() && !still_referenced {
        let _ = fs::remove_dir_all(&dir);
        removed = true;
    }
    Ok(removed)
}

pub fn cmd_skill_remove(name: &str, query: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    if !depin_skill(&home, name, query)? {
        bail!("skill '{query}' not found on '{name}'");
    }
    Ok(())
}
```

- [ ] **Step 4: Run the new + existing skill tests**

Run: `… cargo test -p mur-core depin_` then `… cargo test -p mur-core --lib cmd::agent::skill`
Expected: the three `depin_` tests PASS; pre-existing `skill` tests still PASS (no regression).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill.rs
git commit -m "feat(agent): depin_skill removes refs + installed_skills cards + on-disk dir"
```

---

### Task 2: Fix `shadow-drift` remediation + `fixable` flags

**Files:**
- Modify: `mur-core/src/cmd/skill_doctor.rs` (`run_shadow_drift`)
- Test: `mur-core/src/cmd/skill_doctor.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: the existing `run_shadow_drift` from S2.
- Produces: identical-content shadow findings now carry `fixable: true` and remediation `mur agent skill remove <agent> <name>` (basename, no `skills/` prefix); diverged findings carry `fixable: false`. Task 3's repair relies on both: it only sees fixable findings and parses that remediation string.

- [ ] **Step 1: Update the S2 shadow tests to assert the new contract**

In `mur-core/src/cmd/skill_doctor.rs` tests, extend the two existing shadow tests (add these assertions):

```rust
// in shadow_drift_flags_identical_agent_copy_as_ok, after finding `f`:
assert!(f.fixable, "identical shadow must be auto-fixable");
assert_eq!(
    f.remediation.as_deref().unwrap(),
    "mur agent skill remove a1 foo",
    "remediation must be the basename form depin_skill resolves"
);

// in shadow_drift_flags_diverged_agent_copy_as_warn, after finding `f`:
assert!(!f.fixable, "diverged shadow must NOT be auto-fixable");
```

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-core shadow_drift`
Expected: FAIL — remediation still `skills/foo` and `fixable` still `false` for the identical case.

- [ ] **Step 3: Update `run_shadow_drift`**

In `run_shadow_drift`, change the remediation to the basename form and set `fixable` per branch. Replace the shared `remediation` line and the two `Finding` constructions' `fixable` fields:

```rust
            let remediation = Some(format!("mur agent skill remove {agent_name} {name}"));
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
                    fixable: true,
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
```

- [ ] **Step 4: Run to verify pass**

Run: `… cargo test -p mur-core shadow_drift`
Expected: PASS (all shadow tests including the new assertions).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/skill_doctor.rs
git commit -m "feat(doctor): shadow-drift remediation uses basename + marks identical shadows fixable"
```

---

### Task 3: `ShadowDepinRepair` for `mur skill doctor --fix`

**Files:**
- Create: `mur-core/src/skill_repair/shadow_depin.rs`
- Modify: `mur-core/src/skill_repair/mod.rs` (add `pub mod shadow_depin;`)
- Modify: `mur-core/src/cmd/skill_doctor.rs` (add the repair to the `repairs` vec at ~line 222)
- Test: `mur-core/src/skill_repair/shadow_depin.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `depin_skill` (Task 1), `Repair`/`RepairCtx`/`RepairOutcome`, `Finding`/`Severity`.
- Produces: `pub struct ShadowDepinRepair;` implementing `Repair` with `check_id() == "shadow-drift"`.

- [ ] **Step 1: Write the repair with a failing test**

Create `mur-core/src/skill_repair/shadow_depin.rs`:

```rust
//! Repair impl for `shadow-drift` findings: de-pin an agent-local vendored
//! copy that is byte-identical to the global builtin. Only identical shadows
//! are marked `fixable` (see `run_shadow_drift`), so this repair never touches
//! a diverged copy that might carry a real local edit.

use crate::cmd::skill_doctor::Finding;
use crate::skill_repair::{Repair, RepairCtx, RepairOutcome};

pub struct ShadowDepinRepair;

/// Recover `(agent, skill)` from the finding's own remediation string
/// (`mur agent skill remove <agent> <skill>`) — an internal contract with
/// `run_shadow_drift`, which generates exactly that form.
fn parse_agent_skill(remediation: &str) -> Option<(String, String)> {
    let rest = remediation.strip_prefix("mur agent skill remove ")?;
    let mut it = rest.split_whitespace();
    Some((it.next()?.to_string(), it.next()?.to_string()))
}

impl Repair for ShadowDepinRepair {
    fn check_id(&self) -> &'static str {
        "shadow-drift"
    }

    fn applicable(&self, finding: &Finding) -> bool {
        finding.check_id == "shadow-drift" && finding.fixable
    }

    fn run(&self, finding: &Finding, ctx: &RepairCtx, apply: bool) -> RepairOutcome {
        let Some((agent, skill)) = finding
            .remediation
            .as_deref()
            .and_then(parse_agent_skill)
        else {
            return RepairOutcome::Skipped("no parseable remediation".into());
        };
        if !apply {
            return RepairOutcome::DryRun(format!(
                "would de-pin shadow '{skill}' from agent '{agent}'"
            ));
        }
        match crate::cmd::agent::skill::depin_skill(ctx.home, &agent, &skill) {
            Ok(true) => RepairOutcome::Fixed,
            Ok(false) => RepairOutcome::Skipped(format!("'{skill}' already absent on '{agent}'")),
            Err(e) => RepairOutcome::Failed(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::skill_doctor::{Finding, Severity};

    fn shadow_finding() -> Finding {
        Finding {
            check_id: "shadow-drift".into(),
            category: "shadow".into(),
            severity: Severity::Ok,
            skill_name: "mur-compress".into(),
            message: "redundant".into(),
            remediation: Some("mur agent skill remove mur mur-compress".into()),
            fixable: true,
        }
    }

    fn seed_shadow(home: &std::path::Path) {
        let dir = home.join("agents/mur");
        std::fs::create_dir_all(dir.join("skills/mur-compress")).unwrap();
        std::fs::write(
            dir.join("profile.yaml"),
            "name: mur\ndisplay_name: MUR\nmodel_ref: m\nskills:\n  - skills/concierge\ninstalled_skills:\n- name: mur-compress\n  version: 1.0.0\n  publisher: human:mur\n  description: d\n  category: context\n  abstract: a\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("skills/mur-compress/skill.yaml"),
            "name: mur-compress\nversion: 1.0.0\npublisher: human:mur\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: b\n",
        )
        .unwrap();
    }

    #[test]
    fn applicable_only_for_fixable_shadow_findings() {
        let repair = ShadowDepinRepair;
        assert!(repair.applicable(&shadow_finding()));
        let mut not_fixable = shadow_finding();
        not_fixable.fixable = false;
        assert!(!repair.applicable(&not_fixable));
    }

    #[test]
    fn apply_depins_the_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_shadow(home);
        let ctx = RepairCtx {
            home,
            registry_url: "unused",
        };
        let repair = ShadowDepinRepair;
        let finding = shadow_finding();

        // dry-run touches nothing
        assert!(matches!(
            repair.run(&finding, &ctx, false),
            RepairOutcome::DryRun(_)
        ));
        assert!(home.join("agents/mur/skills/mur-compress").exists());

        // apply removes card + dir
        assert!(matches!(
            repair.run(&finding, &ctx, true),
            RepairOutcome::Fixed
        ));
        assert!(!home.join("agents/mur/skills/mur-compress").exists());
        let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
        assert!(!yaml.contains("mur-compress"));
    }
}
```

- [ ] **Step 2: Register the module + run the test (expect module error first)**

Add to `mur-core/src/skill_repair/mod.rs` (near the other `pub mod` lines):

```rust
pub mod shadow_depin;
```

Run: `… cargo test -p mur-core shadow_depin`
Expected: PASS (the repair + tests compile and pass now that Task 1's `depin_skill` exists).

- [ ] **Step 3: Wire the repair into `mur skill doctor --fix`**

In `mur-core/src/cmd/skill_doctor.rs`, add the repair to the `repairs` vec (the `vec![ … ]` at ~line 222):

```rust
        let repairs: Vec<Box<dyn crate::skill_repair::Repair>> = vec![
            Box::new(crate::skill_repair::tool_availability::ToolAvailabilityRepair),
            Box::new(crate::skill_repair::dep_freshness::DepFreshnessRepair),
            Box::new(crate::skill_repair::stats_sidecar::StatsSidecarRepair),
            Box::new(crate::skill_repair::shadow_depin::ShadowDepinRepair),
        ];
```

- [ ] **Step 4: Verify build, clippy, and the full touched-module tests**

Run:
```
… cargo test -p mur-core shadow_depin
… cargo test -p mur-core shadow_drift
… cargo clippy -p mur-core -- -D warnings
```
Expected: all tests PASS; clippy clean (no warnings from the new code).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/skill_repair/shadow_depin.rs mur-core/src/skill_repair/mod.rs mur-core/src/cmd/skill_doctor.rs
git commit -m "feat(doctor): ShadowDepinRepair de-pins identical builtin shadows under --fix --apply"
```

---

## Rollout / Operator steps (post-merge, not code)

After this ships + a new `mur` is installed and `mur sync` writes `mur-native-tools` into `~/.mur/skills/`:
1. Preview: `mur skill doctor --fix` (dry-run) — lists the identical shadows it *would* de-pin on agent `mur`.
2. Apply: `mur skill doctor --fix --apply` — removes the identical shadows (`mur-compress`, `parallel-code`, `video-analyze`, `watch-together`, `mur-native-tools`). Any diverged shadow is reported (Warn), not removed — de-pin those manually after reviewing: `mur agent skill remove mur <name>`.
3. Restart the concierge (Hub-managed) to apply the profile change.
4. `concierge` (identity) and `brainstorming` (registry, not a global builtin) are never flagged or removed.

---

## Self-Review

**Spec coverage:**
- §4.1 complete de-pin (refs + cards + dir) → Task 1 (`depin_skill` + rewired `cmd_skill_remove`). ✅
- §4.2 fix remediation string → Task 2. ✅
- §4.3 `ShadowDepinRepair` + fixable flags → Task 2 (flags) + Task 3 (repair + registration). ✅
- §5 rollout → Rollout section (corrected to `--fix --apply`). ✅

**Placeholder scan:** none — all code and test bodies are complete.

**Type consistency:** `depin_skill(home: &Path, agent: &str, skill: &str) -> Result<bool>` is defined in Task 1 and consumed verbatim in Task 3. `Repair`/`RepairCtx`/`RepairOutcome` match the framework. `Finding` fields (`check_id, category, severity, skill_name, message, remediation, fixable`) match. Remediation format `mur agent skill remove <agent> <skill>` is produced in Task 2 and parsed in Task 3 by the same 4-word shape. `RepairOutcome::Failed(String)` — `e.to_string()` used (anyhow error → String).

**Scope:** three focused tasks, CLI-only. Hub go-forward guard and the pack kernel remain out of scope per the spec. `ponytail:` the repair parses `(agent, skill)` from its own remediation string rather than adding an `agent` field to `Finding` (which would ripple across ~12 construction sites) — internal contract, both ends in-repo, covered by a test.
