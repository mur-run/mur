# Parallel Decomposition Lens — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Author the **governor skill** `parallel-decompose` — a topology classifier that makes parallelism MUR's default decomposition lens (explore / compete / coupled-write / coherence-bound), routing to existing mechanism with deliberate recombination contracts.

**Architecture:** Phase 1 is **skill authoring + validation + dogfood — no new Rust.** The governor is a `SkillManifest` YAML bundled alongside the existing `mur-core/src/skills/parallel_code.yaml`; it classifies a task's topology, routes (explore→`parallel_jobs`; compete→heterogeneous judge/cherry; coupled-write→**delegate to `parallel-code`**; coherence-bound→single writer), and selects the recombination coordinator (the hard part). The mechanism (`parallel_jobs`, fleet/Tier 1 worktrees, the three reconcilers, `parallel-code`) is reused (~95%). The code-heavy layers (named MCP primitives, dedicated orchestrator agent, Workflow templates) are **Phase 2, out of scope here.**

**Tech Stack:** YAML (`SkillManifest` schema), MUR skill loader (`mur_common::skill`), `mur skill` CLI. No new crate dependencies.

## Global Constraints

- **No new crate dependencies** (MUR mandatory rule). Phase 1 adds **no Rust** — reuse existing mechanism only.
- **Schema:** the skill MUST follow the same `SkillManifest` shape as `mur-core/src/skills/parallel_code.yaml` (top-level `name`/`version`/`publisher`/`description`/`category`/`content{abstract,procedure{steps[]}}`/`tags`/`triggers[]{type,pattern}`/`priority`).
- **Brand:** user-visible strings say **MUR** (uppercase); the skill `name` slug is lowercase (`parallel-decompose`).
- **Default-safe:** when a task's topology is ambiguous, the classifier picks the **safer/cheaper** branch (single-writer for writes, collect-not-reduce for reads).
- **No debate primitive** and **no silent write-merge** — these are design invariants, not options.

---

## File Structure

- `mur-core/src/skills/parallel_decompose.yaml` — **Create.** The governor skill (the whole Phase 1 deliverable).
- Wherever `parallel_code.yaml` is registered/bundled — **Modify** (mirror the same registration for the new skill). Task 1 locates this.
- No test files (the deliverable is skill content; validation is loader + routing spot-check + dogfood).

---

### Task 1: Author the `parallel-decompose` governor skill

**Files:**
- Create: `mur-core/src/skills/parallel_decompose.yaml`
- Modify: the registration site of `parallel_code.yaml` (located in Step 1) — add `parallel_decompose` the same way.

**Interfaces:**
- Consumes: the `SkillManifest` schema (mirror `parallel_code.yaml` exactly).
- Produces: an installed skill named `parallel-decompose` that the retrieve/inject pipeline surfaces on matching triggers.

- [ ] **Step 1: Locate how `parallel_code.yaml` is bundled/registered**

Run: `grep -rn "parallel_code\|parallel-code" mur-core/src/ build.rs mur-core/build.rs 2>/dev/null`
Expected: find the include/registration (e.g. an `include_str!`, a build-time copy into `~/.mur/skills/`, or a bundled-skills list). Note the exact site — Step 4 mirrors it for `parallel_decompose`.

- [ ] **Step 2: Write the skill file**

Create `mur-core/src/skills/parallel_decompose.yaml` with EXACTLY:

```yaml
name: parallel-decompose
version: 0.1.0
publisher: human:mur
description: "Classify ANY task's TOPOLOGY before deciding how to do it — explore (read/gather → fan out, recombine by union or careful synthesis), compete (best-of-N at one goal → fan out, SELECT with an independent judge), coupled-write (interdependent edits → serialize unless disjoint + contract-frozen, escalate overlaps), or coherence-bound (one shared-taste design → single writer). Parallelism as the default decomposition lens, gated read-parallelize / write-serialize."
category: workflow
content:
  abstract: |
    Before deciding HOW to do a task, classify WHY it can (or cannot) be
    parallelized — by its TOPOLOGY, not its domain. The hard part is
    RECOMBINATION, not isolation: worktrees solve filesystem conflicts
    trivially; the risk and the value are both in combining results.

    Four topologies:
    - explore (read/gather: search, find files, find content, collect evidence)
      → outputs are ADDITIVE → fan out freely (no worktree) → recombine by
      union/dedupe (collect) or careful synthesis (reduce).
    - compete (best-of-N at the SAME goal: design variants, N approaches)
      → outputs COMPETE → fan out N heterogeneous attempts → SELECT the best with
      an INDEPENDENT judge. Never merge competing rewrites; never run a debate.
    - coupled-write (editing interdependent code/state) → outputs CONFLICT →
      serialize UNLESS disjoint files + contract frozen first → escalate overlaps.
    - coherence-bound (one design whose pieces share architectural taste, or a
      sequential dependency) → DO NOT parallelize → one coherent writer.

    Grounding: Anthropic multi-agent research (breadth +90%, ~15x tokens, excludes
    most coding); Cognition "Don't Build Multi-Agents" (share CONTEXT not STATE; the
    Flappy-Bird trap of mismatched parallel writers); MAST (~79% of multi-agent
    failures are specification + coordination, impossible single-agent);
    Lost-in-the-Middle (naive concatenation of N results under-attends the middle).
  procedure:
    steps:
      - description: |
          CLASSIFY into one topology. When ambiguous, pick the safer/cheaper
          (single-writer for writes; collect-not-reduce for reads):
          - independent subtasks, additive outputs (find/search/gather) → explore
          - same goal, want the best of several attempts (variants) → compete
          - editing interdependent code/state → coupled-write
          - one coherent design (shared taste) or a sequential dependency → coherence-bound
      - description: |
          ROUTE by topology:
          - explore → fan out read-only jobs via parallel_jobs (no worktree needed).
          - compete → fan out N HETEROGENEOUS attempts (different model/approach,
            not personas); archive the losing variants.
          - coupled-write → DELEGATE to the parallel-code skill's gate (disjoint files
            + contract frozen read-only first + no sequential dependency + >=3 units +
            worth ~3-15x tokens) — else single writer.
          - coherence-bound → say so in one line and do it single-threaded.
          Execution shape: known decomposition → a saved Workflow; emergent/dynamic
          decomposition → orchestrator + parallel_jobs; not-parallel → single agent.
      - description: |
          RECOMBINE — choose the coordinator deliberately (this is the hard part):
          - explore.collect → union the results and dedupe by content; return records.
          - explore.reduce (synthesize an ANSWER) → rank/dedupe FIRST, then synthesize
            iteratively and structurally (per-source records), NOT one concatenated
            blob — this avoids Lost-in-the-Middle degradation.
          - compete → score with an INDEPENDENT, heterogeneous judge (a different
            model, or a grounded/external check) and SELECT; never a debate.
          - coupled-write → disjoint hunks auto-merge; ANY overlap ESCALATES to
            judge/human. A semantic conflict is a correctness problem, not a merge one.
      - description: |
          GUARDRAILS (always):
          - Share CONTEXT, not STATE: workers get shared goals/conventions/contracts
            read-only + isolated working files; share selectively, not maximally.
          - Give each worker a SPECIFIC boundary (no vague delegation → no duplicated work).
          - For writes: freeze the contract + assign explicit ownership BEFORE fan-out.
          - VERIFY independently before marking done.
          - Economics: only fan out >=3 independent units worth the ~4-15x token premium;
            cap fan-out width; budget + kill-switch stay external (daemon).
tags: [parallel, decomposition, orchestration, topology, fan-out, single-writer]
triggers:
  - type: keyword
    pattern: "(find|search|gather|collect|look up|research).*(across|multiple|several|all|each|every)"
  - type: keyword
    pattern: "(design|generate|draft|propose).*(variant|alternative|option|approach|version)"
  - type: keyword
    pattern: "in parallel|fan.?out|split.*across|best.of.n|at the same time"
priority: normal
```

- [ ] **Step 3: Validate the YAML parses against the schema**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo test -p mur-common --lib skill 2>&1 | tail -5`
Expected: PASS (the SkillManifest parser/round-trip tests still pass; the new file is schema-shaped like `parallel_code.yaml`).
If a manual parse check is preferable: `cargo run -p mur-core -- skill show parallel-decompose` (after Step 4) must print the skill without a parse error.

- [ ] **Step 4: Register the skill the same way `parallel_code.yaml` is**

Mirror the site found in Step 1 (e.g. add `parallel_decompose` next to `parallel_code` in the bundled-skills list / `include_str!` / build copy). Show the exact edit, e.g.:

```rust
// if there is a list like:
//   const BUNDLED_SKILLS: &[(&str, &str)] = &[
//       ("parallel-code", include_str!("skills/parallel_code.yaml")),
//   ];
// add:
        ("parallel-decompose", include_str!("skills/parallel_decompose.yaml")),
```

(If `parallel_code.yaml` is instead copied into `~/.mur/skills/` by a build/install step, mirror that copy for `parallel_decompose.yaml` — no code edit needed beyond the build list.)

- [ ] **Step 5: Build and confirm the skill loads**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo run -p mur-core -- skill show parallel-decompose 2>&1 | head -20`
Expected: prints the `parallel-decompose` skill (name, description, abstract, procedure) — proving it parses, registers, and loads.

- [ ] **Step 6: Confirm it co-exists with `parallel-code` (no loader regression)**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo run -p mur-core -- skill show parallel-code 2>&1 | head -5`
Expected: still prints `parallel-code` unchanged. Then `cargo test -p mur-common -p mur-core --lib skill 2>&1 | tail -5` → PASS.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/skills/parallel_decompose.yaml mur-core/src/   # + the registration file from Step 4
git commit -m "feat(parallel): parallel-decompose governor skill (Phase 1 keystone)"
```

---

### Task 2: Routing validation on a task corpus

**Files:** none created — this is a behavioral spot-check that the classifier routes representative tasks correctly. (No deterministic unit test; topology classification is model behavior.)

**Interfaces:**
- Consumes: the installed `parallel-decompose` skill from Task 1.
- Produces: a recorded routing-accuracy spot-check (pass/fail per case), appended to the spec's validation section.

- [ ] **Step 1: Write the corpus of representative tasks**

Create a scratch file (not committed) `parallel-routing-cases.md` with one task per row and its EXPECTED topology:

```
| task                                                              | expected     |
| "find every call site of `create_tracks` across the workspace"    | explore      |
| "search these 4 docs for the retrieval scoring formula"           | explore      |
| "design 3 async patterns for this API and pick the best"          | compete      |
| "draft 3 CLI layouts as variants"                                 | compete      |
| "rename this type and update all its dependents"                  | coupled-write (escalate; likely single-writer — shared type) |
| "add error handling to these 20 independent endpoints"            | coupled-write → parallel-code gate (disjoint, contract-frozen) |
| "design a CLI and its underlying library together"                | coherence-bound (single writer) |
```

- [ ] **Step 2: Run each case past the governor and record the routing**

For each row, in an agent session with the skill active, state the task and capture which topology the governor selects and which route it proposes. Record actual-vs-expected.
Expected: each case routes to its expected topology; the coherence-bound and shared-type cases are REFUSED naive parallelism (single writer / escalate).

- [ ] **Step 3: Record results in the spec**

Append a short "Phase 1 routing validation" subsection to `docs/superpowers/specs/2026-06-30-parallel-decomposition-lens-design.md` (the §8 testing section) with the case table + pass/fail. If any case mis-routes, tighten the Task-1 classifier wording (Step 1 of Task 1) and re-run — small wording fix, not a redesign.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-30-parallel-decomposition-lens-design.md
git commit -m "docs(parallel): Phase 1 routing-validation results for parallel-decompose"
```

---

### Task 3: Live dogfood (explore path end-to-end)

**Files:** none — validates the explore route end-to-end on a real task, reusing the proven Tier 1 / `parallel_jobs` mechanism.

**Interfaces:**
- Consumes: the `parallel-decompose` skill + `parallel_jobs` MCP + (optionally) fleet.
- Produces: a recorded end-to-end run proving classify → fan-out → recombine works for a real explore task.

- [ ] **Step 1: Pick a real read-only explore task**

E.g. "find every place the three reconcilers (judge/cherry, partition, concurrent) are invoked across `mur-core`." This is genuinely additive (collect/dedupe), zero write-conflict risk.

- [ ] **Step 2: Run it with the governor active**

In an agent session (or fleet), let the governor classify it `explore`, fan out via `parallel_jobs`, and recombine by collect/dedupe.
Expected: the agent proposes/uses an explore fan-out (not a single linear search), and the recombination dedupes overlapping hits into one record set.

- [ ] **Step 3: Record the run**

Append the dogfood outcome (what was classified, how it fanned out, recombination quality) to the spec's §8. Note any gap (e.g. dedupe missed near-duplicates → a candidate for the Phase-2 code aggregator).

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-06-30-parallel-decomposition-lens-design.md
git commit -m "docs(parallel): Phase 1 explore-path dogfood result"
```

---

## Self-Review

**Spec coverage:** §2 classifier → Task 1 Step 2 (CLASSIFY step). §3 layered encoding → Task 1 (skill = governor; routing names Workflow/orchestrator/parallel_jobs). §4 recombination contracts → Task 1 RECOMBINE step (explore collect/reduce, compete independent judge, write escalate). §5 guardrails → Task 1 GUARDRAILS step. §6 scope (Phase 1 = governor skill, reuse) → whole plan; Phase 2 explicitly excluded. §8 testing → Tasks 2 + 3. Covered.

**Placeholder scan:** the skill YAML is given verbatim (no TBD). Task 1 Step 1/4 are concrete investigative/registration steps (locate-then-mirror), not placeholders. No "add error handling"-style gaps.

**Type consistency:** skill `name` = `parallel-decompose` everywhere (file, registration, `skill show`, commits). Topology names (explore / compete / coupled-write / coherence-bound) are identical across the spec, the skill abstract, and the corpus.

**Note on scope:** this Phase intentionally has no Rust deliverable — it is the judgment keystone, reusing the existing mechanism. If during Task 3 the dedupe gap proves material, the Phase-2 `explore` code aggregator is the follow-on (separate plan), not in-scope here.
