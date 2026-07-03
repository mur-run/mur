# Daily-Jobs Skills with Progressive Disclosure — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `visibility: on_demand` progressive-disclosure mechanism to MUR skills, then ship 13 new built-in skills (4 indexed hubs + 8 hidden leaves + 1 guide) covering the four zero-coverage domains, plus slim the two L0/L1-bloated parallel-* skills.

**Architecture:** Phase A (mechanism, PR 1): new serde-defaulted `Visibility` enum on `SkillManifest`, filtered at the two injection surfaces (`inject/index.rs` learning index, `mur-agent-runtime` Layer-2 injector), surfaced in `mur skill list`, linted by a new `mur skill doctor` "disclosure" check. Phase B (content, PR 2, stacked): author the 13 YAMLs, slim `parallel-decompose`/`parallel-code`, register everything in the built-in sync array, add a budgets regression test.

**Tech Stack:** Rust edition 2024, serde/serde_yaml_ng, schemars (schema is auto-derived — no manual schema edits), cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-07-03-daily-jobs-skills-design.md`

## Global Constraints

- Disclosure budgets (spec §4): `description` ≤ 120 chars; `content.abstract` ≤ 50 words; body ≤ 150 lines; every new body footer carries: `Ground truth: \`mur <cmd> --help\` · Full tutorial: https://app.mur.run/tutorials/mur-daily-jobs-cookbook.html`
- New manifest field is exactly `visibility` with serde values `indexed` (default, omitted on serialize) / `on_demand`.
- Legacy manifests must parse unchanged (serde default; repo has no `deny_unknown_fields` — verified).
- Brand is uppercase **MUR** in all user-visible text; skill `name:` slugs stay lowercase.
- Tests: `cargo nextest run -p <crate>` (plain `cargo test -p <crate>` also fine). Gates: `cargo clippy --all --no-deps --locked -- -D warnings` and `cargo fmt --all -- --check`. No special env vars needed for these crates.
- Two stacked PRs. **Merge-commit (never squash) PR 1 if PR 2 is stacked on it** — squash severs the child branch's ancestry.
- Trigger `type:` values are snake_case (`command|keyword|session_start|manual`) — confirm against `mur-common/src/skill/types.rs:92` before authoring YAML.
- Phase A branch: `feat/skill-visibility` (created from `feat/daily-jobs-skills`, which holds the spec commit). Phase B branch: `feat/daily-jobs-skills-content` (created from `feat/skill-visibility`).

---

## Phase A — mechanism (PR 1)

### Task 1: `Visibility` enum + `SkillManifest.visibility` field

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` (enum near `SkillScope` at line ~9; field inside `SkillManifest` after the `scope` block at line ~110; test in `#[cfg(test)]` at line ~393)

**Interfaces:**
- Produces: `mur_common::skill::manifest::Visibility` enum (`Indexed` default / `OnDemand`), `Visibility::is_indexed(&self) -> bool`, public field `SkillManifest.visibility: Visibility`. Tasks 2–4 consume these exact names.

- [ ] **Step 1: Write the failing test** (add inside the existing `mod tests`)

```rust
    #[test]
    fn visibility_defaults_to_indexed_and_parses_on_demand() {
        let yaml = r#"
name: vis-default
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
"#;
        let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(m.visibility, Visibility::Indexed);

        let yaml2 = format!("{yaml}visibility: on_demand\n");
        let m2: SkillManifest = serde_yaml_ng::from_str(&yaml2).unwrap();
        assert_eq!(m2.visibility, Visibility::OnDemand);

        // Default is omitted on serialize (keeps existing manifests signature-stable).
        let out = serde_yaml_ng::to_string(&m).unwrap();
        assert!(!out.contains("visibility"));
        let out2 = serde_yaml_ng::to_string(&m2).unwrap();
        assert!(out2.contains("visibility: on_demand"));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-common visibility_defaults`
Expected: compile error — `Visibility` not found.

- [ ] **Step 3: Implement** (place the enum right after the `SkillScope` impl, ~line 33; mirror `SkillScope`'s derives)

```rust
/// Progressive disclosure: whether a skill appears in the always-injected
/// learning index or is loadable on demand only.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Listed in the session-start learning index (default).
    #[default]
    Indexed,
    /// Excluded from the index and Layer-2 abstract injection; reachable via
    /// `mur skill show`, search, and retrieval.
    OnDemand,
}

impl Visibility {
    /// Returns `true` if this is the default `Indexed` visibility.
    pub fn is_indexed(&self) -> bool {
        matches!(self, Visibility::Indexed)
    }
}
```

Field, inserted directly after the `scope` field block in `SkillManifest`:

```rust
    /// Progressive disclosure: `on_demand` skills never appear in the
    /// session-start learning index or Layer-2 abstract injection.
    #[serde(default, skip_serializing_if = "Visibility::is_indexed")]
    pub visibility: Visibility,
```

Note: every construction site of `SkillManifest { .. }` struct literals (if any outside tests use exhaustive init) needs `visibility: Visibility::default()` — the compiler will list them.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-common`
Expected: PASS (including the new test). `mur skill schema` output picks the field up automatically via `schemars::schema_for!` — no schema edits.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/manifest.rs
git commit -m "feat(skill): add visibility field (indexed|on_demand) to SkillManifest"
```

### Task 2: exclude on-demand skills from the learning index

**Files:**
- Modify: `mur-core/src/inject/index.rs` (`build_from_skills`, filter at line ~105; test in `mod tests` at line ~127)

**Interfaces:**
- Consumes: `Visibility` from Task 1; `crate::retrieve::skill_candidates::LoadedSkill { manifest, stats }`; `mur_common::skill::stats::SkillStats::new(name, version, digest, now)`.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn build_from_skills_excludes_on_demand() {
        use crate::retrieve::skill_candidates::LoadedSkill;
        use mur_common::skill::parse_canonical;
        use mur_common::skill::stats::SkillStats;

        let mk = |name: &str, extra: &str| {
            let yaml = format!(
                r#"name: {name}
version: 0.1.0
publisher: human:t
description: test skill
category: context
content:
  abstract: a
  context: b
{extra}"#
            );
            let manifest = parse_canonical(&yaml).unwrap();
            let stats = SkillStats::new(name, "0.1.0", "digest", chrono::Utc::now());
            LoadedSkill { manifest, stats }
        };
        let skills = vec![mk("visible", ""), mk("hidden", "visibility: on_demand\n")];
        let idx = build_from_skills(&skills, None);
        let names: Vec<_> = idx.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["visible"]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core build_from_skills_excludes`
Expected: FAIL — `hidden` present in entries.

- [ ] **Step 3: Implement** — extend the existing filter chain in `build_from_skills`:

```rust
        .filter(|s| s.stats.lifecycle_state != LifecycleState::Archived)
        .filter(|s| {
            s.manifest.visibility != mur_common::skill::manifest::Visibility::OnDemand
        })
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core inject::index`
Expected: PASS (new + existing `format_l0`/save/load tests).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/inject/index.rs
git commit -m "feat(inject): learning index skips visibility: on_demand skills"
```

### Task 3: exclude on-demand skills from runtime Layer-2 injection

**Files:**
- Modify: `mur-agent-runtime/src/skills/injector.rs` (`inject_layer2` filter chain at lines ~53–74; tests at line ~132)

**Interfaces:**
- Consumes: `Visibility` from Task 1; the file's existing `loaded(name, abstract_, trust, triggers)` test fixture (line ~139) — its `triggers` parameter appends raw root-level YAML, so it can carry the `visibility:` line too.

- [ ] **Step 1: Write the failing test** (copy the `inject_layer2` call pattern — argument order and count — from the existing test `project_scoped_skill_injects_only_when_project_matches` in this same file; only the skill fixture differs)

```rust
    #[test]
    fn on_demand_skill_never_injects_layer2() {
        let s = loaded(
            "hidden-leaf",
            "should never appear",
            TrustLevel::Verified,
            "visibility: on_demand\ntriggers:\n  - type: session_start\n    pattern: \"\"",
        );
        // Call inject_layer2 exactly as the existing scope test does (same
        // budget and None scope args); assert nothing was injected:
        // assert!(result.injected_names.is_empty());
        // assert!(result.system_addendum.is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-agent-runtime on_demand_skill_never`
Expected: FAIL — the skill injects (has a session_start trigger and Verified trust).

- [ ] **Step 3: Implement** — add one clause to the `inject_layer2` filter chain, directly after the `scope_visible` filter:

```rust
        .filter(|s| {
            s.manifest.visibility != mur_common::skill::manifest::Visibility::OnDemand
        })
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-agent-runtime skills::injector`
Expected: PASS, existing injector tests untouched.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/skills/injector.rs
git commit -m "feat(runtime): Layer-2 injector skips visibility: on_demand skills"
```

### Task 4: `skill list` marker + doctor "disclosure" lint

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs` (`cmd_list`, lines ~93–113)
- Modify: `mur-core/src/cmd/skill_doctor.rs` (`all_checks` array ~line 140; dispatch match ~line 177; new check fn + tests at end of file)

**Interfaces:**
- Consumes: `Visibility` from Task 1; `local::load_installed(&home, name) -> Result<SkillManifest>`; `load_manifest(&ctx.home, skill_name) -> Option<SkillManifest>` (skill_doctor.rs:282); `Finding`/`Severity::Warn` (skill_doctor.rs:15–34).
- Produces: pure helper `disclosure_findings(m: &SkillManifest, skill_name: &str) -> Vec<Finding>` (unit-tested); check id string `"disclosure"`.

- [ ] **Step 1: Write the failing test** (in `skill_doctor.rs`, add a `#[cfg(test)] mod tests` if none exists)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(desc: &str, abstract_: &str) -> mur_common::skill::manifest::SkillManifest {
        let yaml = format!(
            r#"name: t
version: 0.1.0
publisher: human:t
description: "{desc}"
category: context
content:
  abstract: "{abstract_}"
  context: b
"#
        );
        mur_common::skill::parse_canonical(&yaml).unwrap()
    }

    #[test]
    fn disclosure_flags_fat_description_and_abstract() {
        let fat_desc = "d".repeat(121);
        let fat_abs = vec!["word"; 51].join(" ");
        let f = disclosure_findings(&manifest(&fat_desc, &fat_abs), "t");
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.check_id == "disclosure" && x.severity == Severity::Warn));

        let ok = disclosure_findings(&manifest("short", "brief abstract"), "t");
        assert!(ok.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core disclosure_flags`
Expected: compile error — `disclosure_findings` not defined.

- [ ] **Step 3: Implement**

In `skill_doctor.rs`: add `"disclosure"` to the `all_checks` array; add the match arm

```rust
                "disclosure" => {
                    findings.extend(run_disclosure(&ctx, skill_name));
                }
```

and the functions:

```rust
fn run_disclosure(ctx: &DoctorCtx, skill_name: &str) -> Vec<Finding> {
    match load_manifest(&ctx.home, skill_name) {
        Some(m) => disclosure_findings(&m, skill_name),
        None => Vec::new(),
    }
}

/// Progressive-disclosure budgets (spec 2026-07-03 §4): description ≤ 120
/// chars, abstract ≤ 50 words. Warn-only — third-party skills stay valid.
fn disclosure_findings(
    m: &mur_common::skill::manifest::SkillManifest,
    skill_name: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let desc_chars = m.description.chars().count();
    if desc_chars > 120 {
        findings.push(Finding {
            check_id: "disclosure".into(),
            category: "disclosure".into(),
            severity: Severity::Warn,
            skill_name: skill_name.to_string(),
            message: format!(
                "description is {desc_chars} chars (budget 120) — the index line should say when to reach for the skill, not how"
            ),
            remediation: Some(
                "shorten description; move detail into content.abstract or the body".into(),
            ),
            fixable: false,
        });
    }
    let words = m.content.r#abstract.split_whitespace().count();
    if words > 50 {
        findings.push(Finding {
            check_id: "disclosure".into(),
            category: "disclosure".into(),
            severity: Severity::Warn,
            skill_name: skill_name.to_string(),
            message: format!(
                "abstract is {words} words (budget 50) — sink methodology into the body"
            ),
            remediation: Some(
                "trim content.abstract to scope + one caveat + a load hint".into(),
            ),
            fixable: false,
        });
    }
    findings
}
```

In `skill_cmd.rs` `cmd_list`, replace the `if local::load_installed(...).is_ok()` branch:

```rust
        match local::load_installed(&home, name) {
            Ok(m) => {
                let marker = if m.visibility
                    == mur_common::skill::manifest::Visibility::OnDemand
                {
                    "  [on-demand]"
                } else {
                    ""
                };
                println!("{name:30} [{level:?}]{marker}");
            }
            Err(_) => {
                println!(
                    "{name:30} [{level:?}]  ⚠ invalid: no readable manifest (run `mur skill remove {name}`)"
                );
            }
        }
```

- [ ] **Step 4: Run tests + manual check**

Run: `cargo nextest run -p mur-core skill_doctor`
Expected: PASS.
Run: `cargo run -- skill doctor parallel-decompose --check disclosure`
Expected: two Warn findings (464-char description, 198-word abstract) — proves the lint bites; these get fixed in Task 6.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/skill_cmd.rs mur-core/src/cmd/skill_doctor.rs
git commit -m "feat(skill): [on-demand] list marker + doctor disclosure lint"
```

### Task 5: Phase A gates + PR 1

- [ ] **Step 1: Run full gates**

```bash
cargo nextest run -p mur-common -p mur-core -p mur-agent-runtime
cargo clippy --all --no-deps --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: all green (run `cargo fmt --all` first if fmt complains, then re-stage).

- [ ] **Step 2: Push + PR**

```bash
git push -u origin feat/skill-visibility
gh pr create --base main --head feat/skill-visibility \
  --title "feat(skill): progressive disclosure — visibility: on_demand" \
  --body "SkillManifest gains a serde-defaulted visibility field (indexed|on_demand). on_demand skills are excluded from the session-start learning index (inject/index.rs) and runtime Layer-2 abstract injection, remain reachable via skill show/search, show an [on-demand] marker in skill list, and a new 'disclosure' doctor lint warns on fat index lines (desc>120 chars) and abstracts (>50 words). Spec: docs/superpowers/specs/2026-07-03-daily-jobs-skills-design.md. Content PR follows stacked."
```
Note: **merge with a merge commit, not squash** (PR 2 stacks on this branch).

---

## Phase B — content (PR 2, branch `feat/daily-jobs-skills-content` off `feat/skill-visibility`)

Shared conventions for every YAML authored below (Tasks 6–10):

- Copy `version:` and `publisher:` conventions verbatim from the existing `mur-core/src/skills/mur_context.yaml` (read it first; built-ins share one publisher string).
- `category: context` for all skills except `parallel-topology-guide` (`category: note`, body under `content.note`).
- Leaves and the guide carry `visibility: on_demand`; hubs carry no `visibility:` key (default indexed).
- Bodies are Markdown in `content.context`: `## <section>` per daily job, command tables (`command | when | gotcha`), ≤150 lines, and the L3 footer line from Global Constraints.
- Hubs list their leaves explicitly: `Deep-dive: run \`mur skill show <leaf>\`.`
- Every command line MUST match the cookbook (`docs/tutorials/mur-daily-jobs-cookbook.html`, English text) — it is code-sourced; if a discrepancy is found, `mur <cmd> --help` wins and the cookbook gets a follow-up fix note.

### Task 6: `parallel-topology-guide` + slim the two violators

**Files:**
- Create: `mur-core/src/skills/parallel_topology_guide.yaml`
- Modify: `mur-core/src/skills/parallel_decompose.yaml`, `mur-core/src/skills/parallel_code.yaml`

**Interfaces:**
- Produces: skill names `parallel-topology-guide` (on_demand), slimmed `parallel-decompose`/`parallel-code`. Task 10 cross-links these names; Task 11 registers the guide.

- [ ] **Step 1: Move methodology, slim L0/L1**

1. Read both existing YAMLs. Cut the methodology prose out of their `description`/`content.abstract` and paste it VERBATIM (content preserved, layer changed) into the new guide under `## From parallel-decompose` / `## From parallel-code` headings in `content.note`.
2. Replace the two skills' L0/L1 with exactly:

`parallel_decompose.yaml`:
```yaml
description: Classify a task's topology (explore/compete/coupled-write/coherence-bound) before choosing serial or parallel.
content:
  abstract: >-
    Before decomposing any task, classify its topology: explore (fan out
    reads), compete (best-of-N with a judge), coupled-write (serialize unless
    disjoint), coherence-bound (single writer). Load parallel-topology-guide
    for the full method; mur-parallel-exec maps topologies to MUR commands.
```

`parallel_code.yaml`:
```yaml
description: Decide whether a coding task should fan out across parallel coder sub-agents — default is a single writer.
content:
  abstract: >-
    Propose parallel coders ONLY for disjoint, mechanical, contract-pinned
    work, and only after user approval; otherwise keep one coherent writer.
    Load parallel-topology-guide for the criteria and escalation rules.
```
(Keep each skill's other fields — triggers, category, procedure steps — unchanged.)

3. Guide frontmatter:
```yaml
name: parallel-topology-guide
description: Full methodology behind parallel-decompose and parallel-code — load on demand when applying the topology lens.
category: note
visibility: on_demand
```

- [ ] **Step 2: Validate + lint**

```bash
cargo run -- skill validate mur-core/src/skills/parallel_topology_guide.yaml
cargo run -- skill validate mur-core/src/skills/parallel_decompose.yaml
cargo run -- skill validate mur-core/src/skills/parallel_code.yaml
```
Expected: all valid. (The doctor lint from Task 4 runs against installed skills; the regression test in Task 11 enforces budgets on these files directly.)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/skills/
git commit -m "refactor(skills): sink parallel-* methodology into on-demand topology guide"
```

### Task 7: fleet domain — 1 hub + 2 leaves

**Files:**
- Create: `mur-core/src/skills/mur_fleet_manage.yaml`, `mur-core/src/skills/mur_fleet_loop.yaml`, `mur-core/src/skills/mur_fleet_share.yaml`

**Interfaces:**
- Produces: skill names `mur-fleet-manage` (hub), `mur-fleet-loop`, `mur-fleet-share` (on_demand). Task 11 registers them.

- [ ] **Step 1: Author the three YAMLs**

Hub `mur-fleet-manage` frontmatter:
```yaml
name: mur-fleet-manage
description: Run agent squads on a shared goal — create/show fleets, run iterations, queue jobs; loop/budget/share via leaves.
category: context
triggers:
  - type: keyword
    pattern: "mur fleet"
  - type: keyword
    pattern: "agent squad"
```
Abstract (≤50 words): fleets = named agent squads over one signed `fleet-<name>` channel with a router; members must be running; runs are fail-closed (`yes:false`). Load body for commands; leaves for loops/budgets and sharing/governance.
Body sections + exact commands:
- `## Create & inspect`: `mur fleet create <name> --members pm,qa --router mur --goal "…"`, `list`, `show <name>`, `add/remove <name> <agents…>`, `delete <name> --yes`. Gotchas: name is lowercase slug; channel `fleet-<name>` auto-created; agents survive delete.
- `## Run & queue`: `mur fleet run <name> ["one-shot job"]`, `send <name> "job"`, `jobs <name> --all`. Gotchas: members must be running (`mur agent status`); jobs persist as `~/.mur/fleets/<name>/jobs/<uuidv7>.yaml` FIFO.
- `## Router planning`: automatic inside `run`; validated member/dependency/cycle; invalid plan → broadcast-to-all fallback; observe via `~/.mur/channels/fleet-<name>/events.jsonl`.
- `## Deep-dive`: `mur skill show mur-fleet-loop` (loops/budget/kill-switch/autorun), `mur skill show mur-fleet-share` (bundles/commander).
- L3 footer.

Leaf `mur-fleet-loop` (`visibility: on_demand`, no triggers) sections:
- `## Guarded loop`: `mur fleet run <name> --loop --max-iterations 5 --deadline 30m --budget-usd 5.0`; deadline is a RELATIVE duration (`30s/5m/2h/1d`), not a date; no `--yes` flag exists; guards live outside agents (cap default 8, deadline, stuck-detection, budget).
- `## Convergence`: `mur fleet set-loop <name> --done-when "marker:ALL_GREEN"` — converges only when a member emits the marker alone on its own line this run; unset → router judges DONE/CONTINUE, fail-safe continue.
- `## Budget`: real token accounting (Task usage summed); rate = `MUR_FLEET_COST_PER_1K` env → dearest `models.yaml` output rate → documented default; 0-token iterations fall back to projection (never under-counts).
- `## Kill-switch`: `mur fleet stop <name>` writes `~/.mur/fleets/<name>/.stopped` (running loop bails next iteration, daemon skips, manual run refuses); `start` clears.
- `## Unattended autorun`: `mur fleet set-loop <name> --trigger interval:30m|"cron:0 9 * * 1-5" --budget-usd 3`; safety triad = `MUR_FLEET_AUTORUN=1` (or config `fleet.autorun: true`) + per-fleet budget > 0 + not stopped; `.last_run` stamp; cron fleets baseline-stamped on first sight (never fire spuriously on enable).
- L3 footer.

Leaf `mur-fleet-share` (`visibility: on_demand`) sections:
- `## Share bundles`: `mur fleet export <name> --with-members -o <name>.fleet`; `mur fleet import <file> [--force] [--no-members] [--yes]` — verifies signature, security-scans skills, installs at lowest trust (peer TOFU), regenerates member identities (never copies private keys), never overwrites existing agents, never auto-runs.
- `## Commander governance`: `mur commander pin <pubkey> [--force]`, `status`, `directive <fleet> kill|resume|budget-ceiling --budget-usd 2.50` — Ed25519-signed channel events; kill halts loops + blocks autorun; ceiling 0.0 = hard halt, else min'd with fleet/CLI budget; fail-closed on read error.
- L3 footer.

- [ ] **Step 2: Validate**

```bash
for f in mur_fleet_manage mur_fleet_loop mur_fleet_share; do \
  cargo run -- skill validate mur-core/src/skills/$f.yaml; done
```
Expected: 3× valid.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/skills/mur_fleet_*.yaml
git commit -m "feat(skills): fleet domain hub + loop/share leaves"
```

### Task 8: workflow domain — 1 hub + 2 leaves

**Files:**
- Create: `mur-core/src/skills/mur_workflow_author.yaml`, `mur-core/src/skills/mur_workflow_hitl.yaml`, `mur-core/src/skills/mur_workflow_delegate.yaml`

**Interfaces:**
- Produces: `mur-workflow-author` (hub), `mur-workflow-hitl`, `mur-workflow-delegate` (on_demand). Task 11 registers them.

- [ ] **Step 1: Author the three YAMLs**

Hub `mur-workflow-author` — triggers: keyword `"workflow"`, keyword `"mur workflow"`. Abstract: two workflow kinds exist and only one supports DAG/HITL/delegation — picking wrong silently drops those features; body has the decision table and schemas.
Body sections:
- `## Two kinds (critical)`: flat `~/.mur/workflows/*.yaml` (sequential `Step{order,description,command,needs_approval,on_failure}` — NO channel/risk/delegation, `--channel*` silently ignored) vs workflow SKILL (`~/.mur/skills/<name>/skill.yaml`, `category: workflow`, `content.procedure` — real DAG). `mur workflow run <query>` resolution order: exact flat name → semantic (>0.6) → keyword → workflow-skill fallback.
- `## Run`: `mur workflow run <name> [--fail-fast] [--prompt] [--yes] [--channel <id>|--channel-new]`; `list`, `show <name> --md`, `search <q>`, `new` (interactive). Gotcha: no `--var` flag — `{{name}}` substitution fills only from fleet/server paths.
- `## DAG procedure schema`: step fields `id`, `depends_on: [ids]`, `command`, `intent`, `delegate_to`, `risk`, `on_failure: skip|abort|retry`, `retry: {max_retries, backoff_secs}`, `timeout_secs`; empty `depends_on` = rank 0; ALL same-rank steps run concurrently; cycles/unknown ids rejected at build.
- `## Schedule`: `mur workflow schedule set <name> "0 9 * * 1-5"` / `list|remove|enable|disable` — installs LaunchAgent (macOS) / crontab (Linux) invoking `mur run <name>`; scheduled runs are non-interactive → keep them read-safe.
- `## Deep-dive`: `mur skill show mur-workflow-hitl`, `mur skill show mur-workflow-delegate`.
- L3 footer.

Leaf `mur-workflow-hitl` (`visibility: on_demand`) sections:
- `## Risk tiers`: `read < write < network-egress < spend < destructive < privileged`; `read` = auto (audit only); every other tier = Ask (pauses for approval); `needs_approval: true` coerces to destructive on channel runs.
- `## Channel runs`: gate fires ONLY with a channel: `mur workflow run <skill> --channel-new` (prints `# channel: <id>` to stderr); pending step writes `HitlRequest`, waits 300 s then fail-closed denies.
- `## Approve`: `grep HitlRequest ~/.mur/channels/<id>/events.jsonl | tail -1 | jq .payload.hitl_id` then `mur channel approve <channel_id> <hitl_id> [--deny --reason "…"]`; command SHA-256-pinned at request and re-verified at execute (drift → refuse).
- `## Resume & auto`: rerun with same run id skips steps whose successful ToolResult is already on the channel; `--yes` auto-approves Ask tier (recorded as `auto`).
- L3 footer.

Leaf `mur-workflow-delegate` (`visibility: on_demand`) sections:
- `## Delegate steps`: `delegate_to: <agent>` + `intent: "<sub-goal>"`; ACTIVE only on channel runs; target agent must be running; same-rank delegate steps fan out concurrently.
- `## Trust model`: specialist dials via A2A `channel/delegate` and signs + writes its OWN reply (router never ghost-writes); per-actor Ed25519 verify on fold; one compliant agent can serve N concurrent turns.
- `## Observe`: `~/.mur/channels/<id>/events.jsonl` — `Delegation`, agent replies, `ToolCall/ToolResult`, `StateChange`; retries use distinct idempotency keys.
- L3 footer.

- [ ] **Step 2: Validate** (same `skill validate` loop as Task 7, over the three files)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/skills/mur_workflow_*.yaml
git commit -m "feat(skills): workflow domain hub + hitl/delegate leaves"
```

### Task 9: agent-setup domain — 1 hub + 2 leaves

**Files:**
- Create: `mur-core/src/skills/mur_agent_setup.yaml`, `mur-core/src/skills/mur_agent_mcp_wire.yaml`, `mur-core/src/skills/mur_agent_schedule.yaml`

**Interfaces:**
- Produces: `mur-agent-setup` (hub), `mur-agent-mcp-wire`, `mur-agent-schedule` (on_demand). Task 11 registers them. Boundary note: lifecycle (create/stop/restart/export) stays in the existing `mur-agent-manage`; this hub covers models/secrets/wiring and links to it.

- [ ] **Step 1: Author the three YAMLs**

Hub `mur-agent-setup` — triggers: keyword `"model registry"`, keyword `"api key"`, keyword `"mur model"`. Abstract: cloud agents need a registry entry + secret or they silently degrade to an echo stub; body covers model add, secrets, and verification; leaves cover MCP wiring and schedules.
Body sections:
- `## Model registry`: `mur model add <alias> --provider anthropic --model <id> --secret env:VAR --tier frontier` (secret refs: `env:VAR|keychain:svc/acct|file:/p|cmd:./script`); `mur model list|show|remove`; `mur model prices refresh|show`.
- `## Cloud agent = TWO steps (StubEcho gotcha)`: `mur agent create assistant --provider anthropic --model <id>` THEN `mur agent secret assistant set ANTHROPIC_API_KEY`; a cloud agent without a registry secret silently echoes — diagnose with `mur agent secret <name> list`.
- `## Verify`: `mur agent status|card|logs <name> --tail 100`; `mur agent send <name> '{"role":"user","parts":[{"kind":"text","text":"hi"}]}'` is a raw A2A single turn with NO tools — probe only; real tool use = `mur agent cli <name>` / `murmur`.
- `## Deep-dive`: `mur skill show mur-agent-mcp-wire`, `mur skill show mur-agent-schedule`; lifecycle = `mur-agent-manage` skill.
- L3 footer.

Leaf `mur-agent-mcp-wire` (`visibility: on_demand`) sections:
- `## Local stdio`: `mur agent mcp add <agent> <id> --command npx --arg -y --arg <pkg> [--force]` — auto-syncs the command into the agent's `processes.spawn` allowlist (OS-enforced); `remove|enable|disable|rename`.
- `## Remote`: `mur agent mcp add-remote <agent> <id> <url> [--bearer-env VAR|--bearer-keychain svc/acct]`; OAuth 2.1: `mur agent mcp login <agent> <id>`; registry: `mur agent mcp search <q>` + `registry-add <agent> <server>`.
- `## Audit`: `mur agent mcp list <agent>`, `inspect <agent> --probe` (exit 0 clean / 1 drift / 4 unpinned / 5 missing), `pin <agent> <id>`.
- L3 footer.

Leaf `mur-agent-schedule` (`visibility: on_demand`) sections:
- `## Cron messages`: `mur agent schedule add <agent> --cron "30 9 * * 1-5" --message "…" [--sends-to <agent>]`; `list <agent>`; `next <agent> --count 3`; `remove <agent> <index>` (0-based).
- `## Idle triggers`: `mur agent schedule idle-add <agent> --after-secs 3600 --message "…" [--cooldown-secs 600]`; `idle-list|idle-remove`.
- `## Which scheduler?`: agent schedule = message injection into a running agent; `mur workflow schedule` = run a saved procedure via OS cron; fleet `loop.trigger` + `MUR_FLEET_AUTORUN` = unattended squad runs.
- L3 footer.

- [ ] **Step 2: Validate** (same loop, three files)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/skills/mur_agent_setup.yaml mur-core/src/skills/mur_agent_mcp_wire.yaml mur-core/src/skills/mur_agent_schedule.yaml
git commit -m "feat(skills): agent-setup domain hub + mcp-wire/schedule leaves"
```

### Task 10: parallel-exec domain — 1 hub + 2 leaves

**Files:**
- Create: `mur-core/src/skills/mur_parallel_exec.yaml`, `mur-core/src/skills/mur_parallel_tracks.yaml`, `mur-core/src/skills/mur_parallel_merge.yaml`

**Interfaces:**
- Consumes: skill names from Task 6 (`parallel-topology-guide`) for cross-links.
- Produces: `mur-parallel-exec` (hub), `mur-parallel-tracks`, `mur-parallel-merge` (on_demand). Task 11 registers them.

- [ ] **Step 1: Author the three YAMLs**

Hub `mur-parallel-exec` — triggers: keyword `"parallel"`, keyword `"fan out"`. Abstract: `parallel-decompose` picks the topology; this skill maps each topology to the MUR command that executes it; body holds the matrix and `parallel_jobs` config.
Body sections:
- `## Topology → command matrix` (8 rows, from the cookbook matrix): DAG rank fan-out → workflow-skill same-rank steps; delegation fan-out → `delegate_to` + `--channel-new`; fleet broadcast → `mur fleet run`; routed plan → automatic in `run`; ephemeral fan-out → `parallel_jobs` MCP tool; speculative tracks → `fleet run --worktree` + judge/cherry; partition → `partition-plan`/`merge`; concurrent merge → `merge-concurrent`. Coherence-bound work stays with ONE agent.
- `## parallel_jobs (ephemeral fan-out)`: allowlist REQUIRED in `~/.mur/config.yaml` → `parallel_jobs: {targets: [pm, qa]}` (deny-by-default, checked before any channel is minted); call shape `{jobs: [{description, agent?}], agent?, max_concurrency (1–32, default 8), yes (default false)}`; returns `{channel_id, output}`; vs fleet: N distinct prompts one-shot vs one standing goal loopable.
- `## Deep-dive`: `mur skill show mur-parallel-tracks`, `mur skill show mur-parallel-merge`, `mur skill show parallel-topology-guide`.
- L3 footer.

Leaf `mur-parallel-tracks` (`visibility: on_demand`) sections:
- `## parallel: block (fleet.yaml, hand-edit)`: `mode: speculative`; `tracks: [{name, approach, model?}]`; `judge: {model, rubric: {correctness: 0.40, design: 0.30, maintainability: 0.20, security: 0.10}}`; `pre_filter: [cargo_check|cargo_clippy_deny]`; NO max_concurrency field (auto `min(tracks, cores−2)`).
- `## Run in worktrees`: `mur fleet run <name> --worktree` (or `MUR_PARALLEL_EXEC=1`); one-shot only — incompatible with `--loop`; requires git repo root + `parallel:` block; creates `.worktrees/<track>/` + `.parallel-base` + `tracks.json`; collision guard reports strays into the main checkout; worktrees persist after the run.
- `## Pick a winner`: `mur fleet compare <name> [--unit <prefix>]`, `judge <name> --stats`, `cherry <name> --auto --promote --target <repo>`.
- L3 footer.

Leaf `mur-parallel-merge` (`visibility: on_demand`) sections:
- `## Partition mode`: `parallel.mode: partition` + `partition: {target_file: src/widget.rs}`; preview `mur fleet partition-plan <name>` (from repo root); run `--worktree`; reassemble `mur fleet merge <name> --promote [--target <repo>]` — deterministic, no LLM in the merge.
- `## Concurrent N-way merge (experimental)`: `MUR_PARALLEL_CONCURRENT=1 mur fleet merge-concurrent <name> [--stats] [--promote] [--target <repo>]`; disjoint hunks auto-merge order-independently; overlaps are REPORTED never silently merged; `--promote` refuses on unresolved overlaps, runs `cargo check` in the destination and reverts on failure; results staged in `~/.mur/fleets/<name>/cherry-result/`.
- L3 footer.

- [ ] **Step 2: Validate** (same loop, three files)

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/skills/mur_parallel_*.yaml
git commit -m "feat(skills): parallel-exec domain hub + tracks/merge leaves"
```

### Task 11: register built-ins + budgets regression test + dogfood + PR 2

**Files:**
- Modify: `mur-core/src/cmd/sync_cmd.rs` (the `skills: &[(&str, &str)]` array in `ensure_mur_skill`, lines ~1070–1117; add a test at the end of the file's test module, or create one if absent)

**Interfaces:**
- Consumes: all 13 YAML files from Tasks 6–10 (exact names listed below).

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn new_builtin_skills_parse_and_respect_disclosure_budgets() {
        // (name, yaml, expect_on_demand)
        let cases: &[(&str, &str, bool)] = &[
            ("mur-fleet-manage", include_str!("../skills/mur_fleet_manage.yaml"), false),
            ("mur-fleet-loop", include_str!("../skills/mur_fleet_loop.yaml"), true),
            ("mur-fleet-share", include_str!("../skills/mur_fleet_share.yaml"), true),
            ("mur-workflow-author", include_str!("../skills/mur_workflow_author.yaml"), false),
            ("mur-workflow-hitl", include_str!("../skills/mur_workflow_hitl.yaml"), true),
            ("mur-workflow-delegate", include_str!("../skills/mur_workflow_delegate.yaml"), true),
            ("mur-agent-setup", include_str!("../skills/mur_agent_setup.yaml"), false),
            ("mur-agent-mcp-wire", include_str!("../skills/mur_agent_mcp_wire.yaml"), true),
            ("mur-agent-schedule", include_str!("../skills/mur_agent_schedule.yaml"), true),
            ("mur-parallel-exec", include_str!("../skills/mur_parallel_exec.yaml"), false),
            ("mur-parallel-tracks", include_str!("../skills/mur_parallel_tracks.yaml"), true),
            ("mur-parallel-merge", include_str!("../skills/mur_parallel_merge.yaml"), true),
            ("parallel-topology-guide", include_str!("../skills/parallel_topology_guide.yaml"), true),
            // Slimmed in Task 6 — budgets now enforced forever:
            ("parallel-decompose", include_str!("../skills/parallel_decompose.yaml"), false),
            ("parallel-code", include_str!("../skills/parallel_code.yaml"), false),
        ];
        use mur_common::skill::manifest::Visibility;
        for (name, yaml, on_demand) in cases {
            let m = mur_common::skill::parse_canonical(yaml)
                .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
            assert_eq!(&m.name, name);
            assert_eq!(
                m.visibility == Visibility::OnDemand,
                *on_demand,
                "{name}: wrong visibility"
            );
            assert!(
                m.description.chars().count() <= 120,
                "{name}: description over 120 chars"
            );
            assert!(
                m.content.r#abstract.split_whitespace().count() <= 50,
                "{name}: abstract over 50 words"
            );
            let body = m.manifest_body_text();
            // If no such helper exists, inline: context/note text or empty.
            let body_lines = body.lines().count();
            assert!(body_lines <= 150, "{name}: body {body_lines} lines (budget 150)");
        }
    }
```
Note on `manifest_body_text()`: no such helper exists — replace that line with
`let binding = m.content.context.clone().or(m.content.note.clone()).unwrap_or_default(); let body = binding;`

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core new_builtin_skills_parse`
Expected: FAIL — include_str! files exist (Tasks 6–10) but the test is new; it should PASS if the YAMLs meet budgets, so a failure here pinpoints a budget/parse violation to fix in the YAML. (If all pass immediately, that is the desired outcome — proceed.)

- [ ] **Step 3: Register the 12 new skills** — append to the `skills` array in `ensure_mur_skill` (parallel-decompose/parallel-code are already registered):

```rust
        (
            "mur-fleet-manage",
            include_str!("../skills/mur_fleet_manage.yaml"),
        ),
        ("mur-fleet-loop", include_str!("../skills/mur_fleet_loop.yaml")),
        (
            "mur-fleet-share",
            include_str!("../skills/mur_fleet_share.yaml"),
        ),
        (
            "mur-workflow-author",
            include_str!("../skills/mur_workflow_author.yaml"),
        ),
        (
            "mur-workflow-hitl",
            include_str!("../skills/mur_workflow_hitl.yaml"),
        ),
        (
            "mur-workflow-delegate",
            include_str!("../skills/mur_workflow_delegate.yaml"),
        ),
        (
            "mur-agent-setup",
            include_str!("../skills/mur_agent_setup.yaml"),
        ),
        (
            "mur-agent-mcp-wire",
            include_str!("../skills/mur_agent_mcp_wire.yaml"),
        ),
        (
            "mur-agent-schedule",
            include_str!("../skills/mur_agent_schedule.yaml"),
        ),
        (
            "mur-parallel-exec",
            include_str!("../skills/mur_parallel_exec.yaml"),
        ),
        (
            "mur-parallel-tracks",
            include_str!("../skills/mur_parallel_tracks.yaml"),
        ),
        (
            "mur-parallel-merge",
            include_str!("../skills/mur_parallel_merge.yaml"),
        ),
        (
            "parallel-topology-guide",
            include_str!("../skills/parallel_topology_guide.yaml"),
        ),
```

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-core`
Expected: PASS.

- [ ] **Step 5: Dogfood — the acceptance check from the spec**

```bash
cargo run -- sync            # or the command that invokes ensure_mur_skill; verify with: grep -rn "ensure_mur_skill" mur-core/src | grep -v fn
ls ~/.mur/skills | grep -c mur-fleet     # expect 3 dirs
cargo run -- skill list | grep on-demand # expect 9 lines ([on-demand] marker)
cargo run -- skill show mur-fleet-manage # hub loads with leaf pointers
```
Then start a fresh Claude Code session in this repo: the learning index must grow by exactly **4** lines (the hubs) versus before, and none of the 9 on-demand names may appear in it.

- [ ] **Step 6: Gates + commit + PR 2**

```bash
cargo nextest run -p mur-core -p mur-common -p mur-agent-runtime
cargo clippy --all --no-deps --locked -- -D warnings
cargo fmt --all -- --check
git add mur-core/src/cmd/sync_cmd.rs
git commit -m "feat(skills): register daily-jobs hubs/leaves + disclosure budgets test"
git push -u origin feat/daily-jobs-skills-content
gh pr create --base feat/skill-visibility --head feat/daily-jobs-skills-content \
  --title "feat(skills): daily-jobs domain skills — 4 hubs + 8 on-demand leaves + topology guide" \
  --body "Content half of the progressive-disclosure spec: fleet/workflow/agent-setup/parallel-exec hubs (indexed) with 8 on-demand leaves + parallel-topology-guide; parallel-decompose/parallel-code slimmed to budget; budgets regression test over all new built-ins. Index grows by exactly 4 lines. Spec: docs/superpowers/specs/2026-07-03-daily-jobs-skills-design.md"
```
(Retarget PR 2's base to `main` after PR 1 merges.)

---

## Self-review notes (already applied)

- Spec coverage: §3 mechanism → Tasks 1–3; §3 list marker + §4 lint → Task 4; §5 inventory (13 skills) → Tasks 6–10 + registration Task 11; §6 fixes → Task 6; §7 tests/dogfood/two-PR rollout → Tasks 5, 11; §8 unknown-field risk → resolved (no deny_unknown_fields in repo; noted in Global Constraints).
- Type consistency: `Visibility::{Indexed,OnDemand}` + `is_indexed` used identically in Tasks 1–4 and 11; `disclosure_findings` name consistent between Task 4 impl and test.
- Known judgment calls the executor must NOT re-litigate: hubs have keyword triggers, leaves have none; guide is `category: note`; legacy fat descriptions (mur-run 174, mur-compress 164, video-analyze 131 chars) are grandfathered — the doctor lint warns but the regression test deliberately covers only the 15 files it lists.
