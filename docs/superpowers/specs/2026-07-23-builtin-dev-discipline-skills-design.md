# Built-in Dev-Discipline Skills — Design

**Date:** 2026-07-23
**Status:** Approved (brainstorm complete; awaiting implementation plan)
**Branch:** `feat/builtin-dev-discipline-skills`

## 1. Goal

Internalize the two best public engineering-skill packs — [obra/superpowers](https://github.com/obra/superpowers) (14 skills) and [mattpocock/skills](https://github.com/mattpocock/skills) (~26 active skills) — as MUR built-in skills, curated and merged (去蕪存菁), adapted to MUR's platform reality:

- MUR agents have **no sub-agent spawning, no TodoWrite, no plan mode** — bash + file tools + MUR delegation primitives (fleet, `parallel_jobs`, workflow delegate).
- MUR user experience is the top design principle: zero-setup delivery, zero token cost when unused, zero conflict with users who already run the upstream packs.

Both surfaces are served by one YAML per skill: (a) MUR agent runtime (`inject_layer2` + trigger matcher), (b) the user's AI-tool sessions (learning index via the MUR hook, slash commands via skill sync).

## 2. Sources and licensing

| Source | License | Attribution |
|---|---|---|
| obra/superpowers | MIT © 2025 Jesse Vincent | notice retained |
| mattpocock/skills | MIT © 2026 Matt Pocock | notice retained |

Adaptation is fully permitted. A new `docs/ATTRIBUTIONS.md` carries both MIT notices plus a skill → source mapping table. Each derived YAML notes its sources in a comment header. `publisher: human:mur-official`.

## 3. Decisions (approved during brainstorm)

- **D1 — Delivery: built-in.** New YAMLs in `mur-core/src/skills/`, registered in `ensure_mur_skill` (`cmd/sync_cmd.rs`), installed to `~/.mur/skills/` on sync, trust = Trusted. Not a capability bundle (no MCP/programs needed — capability sugar deferred to Phase 2), not official-catalog (login/license gates are wrong for MIT-derived content).
- **D2 — Surface posture: both surfaces + progressive disclosure + never-shadow detection.** All 16 leaves `visibility: on_demand` (no index line, no Layer-2 abstract; reachable via hub routing, keyword triggers, `mur skill show`, retrieval). One hub skill is `Indexed` with a `session_start` trigger. When the superpowers plugin is detected, the CLI surface hides the hub too; the runtime surface is unaffected.
- **D3 — Curation: merged canon, 1 hub + 16 leaves.** Overlapping territory (TDD, debugging, review, planning, questioning) is merged into a single version per topic — never two parallel variants.
- **D4 — Namespace: `mur-` prefix on every skill name; slash command = skill name, 1:1.** Rationale: hub routing integrity (cross-references must resolve to our versions on machines that also have superpowers or the mattpocock pack installed), collision-proofing the most collision-prone names in the ecosystem (`tdd`, `code-review`, …), and one tab-completion stem consistent with `mur-in`/`mur-out`/`mur-run`. True colon namespacing (`/mur:tdd`) would require shipping MUR as a Claude Code plugin — a new distribution channel; rejected.
- **D5 — Zero schema change.** Existing `Category` (`workflow`/`meta`/`context`), `Visibility`, `SkillScope`, `HostId` cover everything. No `mur-common` enum edits, so no workspace-excluded Tauri crate compile risk.
- **D6 — No-subagent adaptation doctrine** (§5) applied uniformly to every leaf.
- **D7 — Language & style.** Skill bodies in English, MUR house style (didactic bullets with `*Why:*` rationale lines). Trigger regexes bilingual (English + zh-TW), matching the `watch-together` precedent. Brand rendered "MUR" in any user-facing display text; skill names stay lowercase slugs.

## 4. Skill roster (1 hub + 16 leaves)

All leaves: `scope: user`, `hosts: [all]`, `visibility: on_demand`, content mode `context`, keyword + manual triggers. Category is `workflow` unless noted. Disclosure budgets apply to every skill: description ≤ 120 chars, abstract ≤ 50 words, body ≤ 150 lines.

### 4.1 `mur-dev` — hub (category `meta`, **Indexed**, `session_start` + keyword triggers)

Merged from: superpowers `using-superpowers`.

- The 1%-rule adapted: if a discipline skill might apply, load it (`mur skill show <name>`) **before** acting — including before clarifying questions.
- Announce usage ("Using mur-tdd to …"); follow the loaded skill exactly.
- Process-before-implementation ordering: "build X" → `mur-brainstorm` first; "fix this bug" → `mur-debugging` first.
- Condensed red-flag rationalization table (~8 rows) covering the classic evasions ("just a simple question", "the skill is overkill", "I remember this skill", "let me explore first").
- Routing map: one line per leaf (name + when to reach for it).
- Execution-backend note: delegation ladder summary (§5) — where "dispatch a subagent" appears in ported methodology, MUR uses the ladder instead.
- Precedence: user instructions > skills > defaults.
- Boundary note vs. existing parallel skills: `mur-delegate-dev` executes one plan via delegation; `parallel-code`/`parallel-decompose`/`parallel-topology-guide` choose and run parallel topologies. The hub names both and routes accordingly.

### 4.2 `mur-grilling` — questioning primitive

Merged from: mattpocock `grilling` (+ `grill-me` collapsed into triggers).

- One question at a time; multiple questions at once is bewildering.
- Every question ships with the agent's recommended answer the user can accept in a word.
- Fact/decision split: facts discoverable from the environment are looked up, never asked; decisions are always the user's.
- Walk the decision tree resolving dependencies between decisions in order.
- No question cap by design; natural-language steering ("wrap up") is the control surface.
- No action until the user confirms shared understanding.
- murmur surface: emit choices via `suggest_replies` structured options (`{text, description}`), no hand-numbering (house rule from `mur-native-tools`). CLI surface: plain one-question messages.

### 4.3 `mur-brainstorm` — idea → approved design spec

Merged from: superpowers `brainstorming` + mattpocock `grill-with-docs`.

- HARD-GATE: no code, no scaffolding, no implementation action until a design is presented and the user approves — every project, regardless of perceived simplicity.
- Scope gate first: multi-subsystem requests are decomposed into sub-projects before detail questions.
- Explore project context, then question via the `mur-grilling` protocol.
- Propose 2–3 approaches with trade-offs; lead with a recommendation.
- Present the design in sections scaled to complexity; get approval per section.
- Write the spec to `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md` (or the project's own spec convention); self-review for placeholders/consistency/scope/ambiguity, fix inline; user reviews the written spec.
- Vocabulary emerging mid-design routes through `mur-domain-modeling`.
- Terminal state: route to `mur-writing-plans`. (Superpowers' visual companion is dropped.)

### 4.4 `mur-domain-modeling` — glossary + ADRs (category `context`)

Merged from: mattpocock `domain-modeling`.

- `CONTEXT.md` is a glossary and nothing else: term + 1–2 sentence definition + `Avoid:` list; project-specific concepts only.
- `docs/adr/` for decisions. Create files lazily — only when there is something to write.
- Challenge glossary conflicts the moment they appear; sharpen fuzzy/overloaded terms into one canonical; stress-test with invented edge-case scenarios; surface code-vs-claim disagreements.
- Update `CONTEXT.md` inline the moment a term resolves — never batch.
- Offer an ADR only when all three hold: hard to reverse, surprising without context, result of a real trade-off.

### 4.5 `mur-writing-plans` — spec → executable plan

Merged from: superpowers `writing-plans`.

- Audience model: a skilled developer with zero codebase/domain knowledge and poor test-design instincts must be able to execute mechanically.
- File-structure section before tasks: every created/modified file mapped, one responsibility each.
- Task right-sizing: smallest unit that carries its own test cycle and is worth a fresh review gate.
- Bite-sized steps (2–5 min): write failing test / watch it fail / minimal code / watch it pass / commit — separate checkbox steps.
- Mandatory header: goal, architecture, **Global Constraints** copied verbatim, plus a banner naming the execution skill (`mur-delegate-dev` or `mur-executing-plans`).
- Per-task **Interfaces block** (Consumes/Produces with exact signatures) — implementers see only their own task. When a plan is compiled into a MUR workflow DAG, this block becomes the `depends_on` output threading.
- No-placeholders law: "TBD", "add appropriate error handling", "similar to Task N" are plan failures.
- Self-review: spec-coverage walk, placeholder scan, cross-task type-consistency check.

### 4.6 `mur-tickets` — plan/spec → tracer-bullet tickets

Merged from: mattpocock `to-tickets` (+ spec-shape notes from `to-spec`).

- Vertical slice: a narrow but complete path through every layer, demoable on its own, **sized to a single fresh context window**; prefactoring first ("make the change easy, then make the easy change").
- Each ticket declares blocking edges; the frontier (unblocked tickets) is what gets worked.
- Wide-refactor exception — expand–contract: expand ticket, migration-batch tickets (each blocked by expand, CI green throughout), contract ticket blocked by every batch; if batches can't stay green alone, share an integration branch converging on one final integrate-and-verify ticket.
- Quiz the user on granularity/edges before publishing; iterate to approval.
- Publish backends: default local files `docs/tickets/<slug>/NN-<slug>.md` (with `Status:`/`Blocked-by:` header lines); GitHub issues with native blocking links when the repo uses them.
- Ticket bodies: end-to-end behavior from the user's perspective + acceptance-criteria checkboxes; no file paths or code snippets (they go stale), except prototype-sourced decision snippets.
- MUR tie-in: ticket files are directly consumable as fleet job specs (spec-file dispatch pattern).

### 4.7 `mur-executing-plans` — in-context sequential execution

Merged from: superpowers `executing-plans`.

- Read the plan critically first; raise concerns with the user before starting.
- Isolate first: route through `mur-worktree`; never start implementation on main/master without explicit consent.
- Execute task-by-task exactly as written; run each task's stated verifications; tick the plan file's own `- [ ]` checkboxes (durable across compaction — no TodoWrite needed).
- STOP immediately on: blocker, critical plan gap, unclear instruction, repeated verification failure — ask, never guess or force through.
- On completion, route to `mur-finishing-branch`.

### 4.8 `mur-delegate-dev` — plan execution via MUR delegation

Rewritten from: superpowers `subagent-driven-development` (the largest adaptation).

- Precondition (observable predicate): a MUR delegation surface is available — running agents (`mur agent list`), a murmur session, `parallel_jobs`, or workflow delegate. Otherwise fall back to `mur-executing-plans`.
- Roles: the **router** keeps coordination context; **delegates** receive zero history — a curated brief file per task. Pass file **paths** over A2A, never pasted content.
- Status protocol (delegate reply contract): `DONE` / `DONE_WITH_CONCERNS` / `NEEDS_CONTEXT` / `BLOCKED`, each with defined router handling; never re-send an unchanged brief to a stuck delegate.
- Two-stage per-task review (spec compliance, then code quality) via a second delegate, or the `mur-code-review` rubric as a self-review fallback; Critical/Important fixed before proceeding, Minor recorded in the ledger.
- Final whole-branch review; all its findings go to ONE fix pass with the complete list (per-finding fixers are the most expensive observed failure mode upstream).
- Durable ledger file (e.g. `progress.md` per effort, or channel events): append `Task N: complete (commits X..Y, review clean)`; after compaction trust the ledger + git log over memory; never re-dispatch completed tasks.
- Model tiering via `models.yaml` roles: cheap tier for transcription-grade tasks (plan contains complete code), capable tier for review and architecture.
- Never pre-judge findings in a reviewer brief ("do not flag X" = rigging the review).

### 4.9 `mur-tdd` — red/green/refactor + seams

Merged from: superpowers `test-driven-development` + `testing-anti-patterns` + mattpocock `tdd`/`tests.md`/`mocking.md`.

- Iron Law: NO PRODUCTION CODE WITHOUT A FAILING TEST FIRST. Violating the letter is violating the spirit. Code written before its test gets deleted — delete means delete (no keeping as reference, no adapting).
- Seams first: name the public seams under test and confirm with the user before writing any test; test only at pre-agreed seams.
- RED: one minimal test for one behavior; watch it fail; the failure must be the expected kind. A test that passes immediately is testing existing behavior — fix the test.
- GREEN: simplest code that passes; YAGNI; watch it pass with the full suite green and output pristine.
- REFACTOR only on green; no new behavior. Vertical slices: one test → one implementation → repeat; never all-tests-first.
- Anti-patterns: implementation-coupled tests (mocking internals, private methods, side-channel verification); tautological tests (assertion recomputes the expected value the way the code does — take expected values from an independent source); mock only at system boundaries (external APIs, time, randomness) — never your own classes; test-only methods on production types belong in test utilities.
- Bug fix = failing repro test first, always.
- Condensed rationalization table (~6 rows) from upstream baselines.

### 4.10 `mur-debugging` — feedback-loop-first root cause

Merged from: mattpocock `diagnosing-bugs` + superpowers `systematic-debugging`/`root-cause-tracing`.

- Iron Law: NO FIXES WITHOUT ROOT CAUSE INVESTIGATION — especially under time pressure.
- Phase 1 — build a feedback loop; **this is the skill**: a tight (fast, sharp, deterministic) red-capable signal for this exact bug. Tactic ladder: failing test at a reachable seam → curl/HTTP script → CLI + fixture diff → replay a captured trace → throwaway harness → `git bisect run` → differential (old vs new) loop → HITL script as last resort. Cannot build one → stop, list attempts, ask for artifacts/access. Do NOT hypothesize without a loop.
- Non-deterministic bugs: raise the reproduction rate first (loop 100×, stress, inject sleeps).
- Phase 2 — run red, confirm it is the user's exact symptom; minimise until every remaining element is load-bearing. Read errors completely; check recent diffs; in multi-component systems instrument every boundary once.
- Phase 3 — 3–5 ranked falsifiable hypotheses ("if X is the cause, changing Y makes it disappear") before testing any; show the ranking to the user when present.
- Phase 4 — one variable at a time; tag all instrumentation `[DEBUG-<id>]` so cleanup is one grep.
- Phase 5 — regression test **before** the fix, at a correct seam; no correct seam existing is itself a finding — flag the architecture.
- Phase 6 — cleanup checklist; state the winning hypothesis in the commit/PR message.
- 3-strike rule: ≥3 failed fixes → STOP, question the architecture with the user before a 4th attempt.
- Condensed rationalization table (~6 rows).

### 4.11 `mur-code-review` — two-axis review

Merged from: mattpocock `code-review` + superpowers `code-reviewer.md` rubric.

- Pin the review range first (merge-base three-dot diff); validate the ref resolves and the diff is non-empty before anything else.
- Two axes, never merged or cross-ranked: **Standards** (repo-documented standards + the Fowler 12-smell baseline as named judgement-call heuristics; the repo overrides the baseline; skip anything tooling already enforces) and **Spec** (faithfulness to the originating spec/ticket; quote the spec line per finding; no spec → report "no spec available").
- Execution: two delegate turns when a delegation surface is available; otherwise two sequential passes in-context. ≤400 words per axis.
- Output contract: strengths first (accurate praise), then findings as Critical (bugs, security, data loss) / Important (architecture, missing features, test gaps) / Minor (style, docs) with file:line + what/why/how, ending with an explicit verdict: Ready to merge — Yes / No / With fixes.
- Calibration: not everything is Critical; plan deviations flagged for intent confirmation; plan bugs flagged as plan bugs. Reviewer is read-only.
- Never pre-judge findings in a reviewer brief.

### 4.12 `mur-receiving-review` — processing inbound feedback

Merged from: superpowers `receiving-code-review`.

- Pattern: READ all → UNDERSTAND (restate/ask) → VERIFY against the codebase → EVALUATE for this codebase → RESPOND → IMPLEMENT one item at a time, testing each.
- Forbidden: "You're absolutely right!", "Great point!", gratitude, performative agreement. State the fix or show the code.
- Any unclear item → stop entirely; do not implement even the understood subset (items may be related).
- External feedback = suggestions to evaluate, not orders; human partner = trusted, but still no sycophancy.
- YAGNI check on "implement properly" suggestions: grep actual usage; unused → propose removal.
- Push back with technical reasoning when feedback is wrong for this codebase; wrong pushback → factual correction, no apology spiral.
- Implementation order: clarify everything → blocking → simple → complex.

### 4.13 `mur-verification` — evidence before claims

Merged from: superpowers `verification-before-completion`.

- Iron Law: NO COMPLETION CLAIMS WITHOUT FRESH VERIFICATION EVIDENCE in the same message as the claim.
- 5-step gate: IDENTIFY the proving command → RUN it fresh → READ full output/exit code → VERIFY it supports the claim → claim WITH the evidence.
- Claim→evidence table (condensed): tests pass = fresh 0-failure output; build works = exit 0; bug fixed = original symptom retested; regression test = proven red-green cycle; requirements met = line-by-line checklist.
- MUR-specific row: **delegate/agent work completed = verify the VCS diff or channel evidence — never the agent's self-report.** A fleet member emitting a `done_when` marker or replying DONE is a claim, not evidence; check the diff.
- Red flags: "should / probably / seems to", any satisfaction expression before verification, trusting delegate reports, "just this once".

### 4.14 `mur-finishing-branch` — branch completion

Merged from: superpowers `finishing-a-development-branch`.

- Run the full test suite; failures block the menu entirely.
- Detect environment (normal repo / linked worktree / detached HEAD); determine the base branch.
- Present EXACTLY four options — merge locally / push + PR / keep as-is / discard — with no added editorializing (three when detached).
- Merge path: checkout base, pull, merge, RE-RUN tests on the merged result, only then clean up the worktree, then delete the branch. PR path: never remove the worktree. Discard: typed confirmation listing what is destroyed.
- Provenance cleanup: only remove worktrees under `.worktrees/` or `worktrees/`; cd out before `git worktree remove`; `git worktree prune` after.
- Never force-push unrequested.

### 4.15 `mur-merge-conflicts` — conflict resolution

Merged from: mattpocock `resolving-merge-conflicts`.

- See the current state: history + conflicting files.
- Per hunk, find primary sources for both sides' intent (commit messages, PRs, tickets).
- Preserve both intents where possible; where incompatible, pick the side matching the merge's stated goal and note the trade-off. Never invent new behavior. Never `--abort`.
- Run the project's checks (typecheck → tests → format); fix what the merge broke; finish (continue a rebase to completion).

### 4.16 `mur-skill-authoring` — writing MUR skills (category `meta`)

Merged from: superpowers `writing-skills` + `persuasion-principles` + mattpocock `writing-great-skills`.

- Create skills only for non-obvious, cross-project techniques — not one-offs, not anything mechanically enforceable (automate those instead).
- Description is trigger-only ("Use when …" + symptoms/keywords) — never a workflow summary; agents execute the summary and skip the body (tested upstream regression).
- MUR disclosure budgets are the frame: description ≤120 chars, abstract ≤50 words (Layer 2 = identity + triggers), body ≤150 lines; split into hub + on-demand leaves when over budget.
- Match the form to the failure: prohibition + rationalization table for rule-skipping under pressure; positive recipe/contract for output shaping (prohibitions backfire there); REQUIRED structural slot for omissions; observable-predicate conditional for conditional behavior. No nuance clauses; no exemption clauses.
- Compliance toolkit: Iron Law in a code fence; "letter = spirit" clause; delete-means-delete loophole enumeration; rationalization tables built from real observed excuses; red-flag self-check lists.
- Leading words: compact pretrained concepts (tight, red, tracer bullet, frontier) used consistently so priors do the work.
- Prune no-ops sentence-by-sentence ("be thorough" changes nothing; the fix is a stronger word, not more prose).
- Test before ship: fresh-context micro-test with a no-guidance control; read every flagged match manually; variance = weak binding.
- MUR tie-in: these rules govern harvest-proposal curation (`mur session out`) — LLM-provenance skills stay gated until a human curates them to this standard. Bilingual trigger regexes are house style.

### 4.17 `mur-worktree` — pre-work isolation

Merged from: superpowers `using-git-worktrees`.

- Detect existing isolation first (`GIT_DIR` vs `GIT_COMMON`, superproject guard); already isolated → skip creation.
- Prefer the harness's native worktree tool when one exists (never fight the harness); otherwise `git worktree add`.
- Directory policy: explicit instruction > existing `.worktrees/` > existing `worktrees/` > default `.worktrees/` (MUR's fleet-track convention; reconcilers own it).
- MUST pass `git check-ignore` before creating; otherwise add to `.gitignore` and commit first.
- Auto-detect setup (Cargo.toml → build/check; package.json → install; etc.).
- Baseline tests must pass before work starts; red baseline → report and ask (otherwise new bugs are indistinguishable from pre-existing ones).
- Red flags: nested worktrees, skipped ignore check, proceeding on a red baseline. Failure mode prevented: two agents sharing one checkout.
- Paired cleanup: `mur-finishing-branch`.

## 5. No-subagent adaptation doctrine (applies to every leaf)

1. **Default execution backend: in-context sequential.** Two-axis review = two passes; design-it-twice = three sequential designs; research = in-session reading.
2. **Upgrade only on an observable predicate.** When a MUR delegation surface is demonstrably available (running agents via `mur agent list`, a murmur session, `parallel_jobs`, workflow delegate), skills point to the delegation path. Conditions are written as observable predicates, never vague nuance clauses.
3. **TodoWrite → checkboxes in the plan/spec/ticket file.** Durable on disk, survives compaction — an upgrade for MUR agents, not a downgrade.
4. **Plan mode → the `mur-brainstorm` HARD-GATE.** The approval gate is procedural, not a harness mode.
5. **Fan-out territory belongs to the existing `parallel-*` builtins.** Superpowers' `dispatching-parallel-agents` is not ported; the hub routes topology questions to `parallel-decompose`/`parallel-topology-guide`.

## 6. Visibility, detection, and never-shadow mechanics

- **Index cost when idle:** one learning-index line + one ≤50-word abstract (the hub). Leaves cost zero until routed to.
- **Superpowers detection (CLI surface only):** at learning-index build time, detect an installed superpowers plugin by globbing the Claude config plugin directories (path pattern kept as a named constant). Detected → the hub is suppressed from the index/abstract injection for that session. Runtime injection for MUR agents is never suppressed.
- **Config override:** `skills.dev_discipline_index: auto | always | never` in `~/.mur/config.yaml` (default `auto`). `always` ignores detection; `never` suppresses unconditionally.
- **Never-shadow (local):** during `ensure_mur_skill`, if a same-name skill already exists without the `registry:mur-official/<name>` origin stamp (i.e. the user's own), skip installation and log a notice. Skills carrying our origin stamp follow the existing builtin update semantics (origin/origin_hash drift detection already in the manifest).

## 7. Integration points

- 17 new YAMLs: `mur-core/src/skills/mur_{dev,grilling,brainstorm,domain_modeling,writing_plans,tickets,executing_plans,delegate_dev,tdd,debugging,code_review,receiving_review,verification,finishing_branch,merge_conflicts,skill_authoring,worktree}.yaml`.
- Registry entries in `ensure_mur_skill` (`mur-core/src/cmd/sync_cmd.rs`).
- Budget/parse assertions extended in `builtin_skill_tests` (all 17: parse, name match, visibility expectation — hub Indexed, leaves OnDemand — plus the three budget limits).
- Superpowers-detection function + config key + unit tests (temp-dir with/without plugin path; three config states).
- Trigger-regex compile test for the new bilingual patterns.
- `docs/ATTRIBUTIONS.md` (new).
- Docs follow-ups per the Documentation Checklist: `README.md` section, app.mur.run docs page (`mur-server` repo) — tracked in the implementation plan, not this spec.

## 8. Dropped and deferred

**Not ported (with reasons):**

- `dispatching-parallel-agents` — territory owned by existing `parallel-*` builtins.
- `using-superpowers` — reborn as the `mur-dev` hub, not ported as-is.
- `git-guardrails-claude-code` — MUR's sandbox/entitlement layer covers this more strongly; mechanics are Claude-Code-specific.
- `setup-pre-commit`, `migrate-to-shoehorn`, `scaffold-exercises` — JS-ecosystem/personal-toolchain specific.
- `ask-matt`, `setup-matt-pocock-skills` — router role absorbed by the hub; config-seam pattern noted for Phase 2.
- `personal/*`, `in-progress/*`, `deprecated/*` — out of scope.

**Phase 2 candidates:** `wayfinder` (decision-ticket maps; needs a tracker seam), `prototype` (LOGIC branch), `handoff`, `teach`, `improve-codebase-architecture`, an `engineering` capability bundle, a behavioral eval harness for skills.

## 9. Risks and mitigations

- **150-line budget vs. upstream volume** (writing-skills ≈ 3.8k words, SDD ≈ 3.1k): compression keeps enforcement artifacts (iron laws, rationalization tables, status protocols, step gates) and drops narrative/example stacking. If a skill truly cannot fit, split a second leaf (established hub+leaves precedent) — decided per skill at implementation.
- **Concierge noise:** chat-centric agents receive the hub's ~50-word abstract at session start. Acceptable; per-agent skill disable remains the escape hatch.
- **Detection heuristic misses** (superpowers on another harness): config override covers it.
- **Name collisions in `~/.mur/skills/`:** the `mur-` prefix plus the never-shadow skip make this a non-issue in practice.
- **Hub trigger tuning:** keyword regex too broad → noise, too narrow → missed routing; start conservative, adjust from dogfooding.

## 10. Testing

- Extend `builtin_skill_tests::new_builtin_skills_parse_and_respect_disclosure_budgets` for all 17 (parse + visibility + budgets).
- Unit tests: superpowers detection (plugin dir present/absent × config `auto`/`always`/`never`).
- Trigger regex compilation test.
- Behavioral validation via dogfooding in murmur (does the hub route correctly; do keyword triggers fire) — no eval harness in Phase 1 (YAGNI).

## 11. Attribution

This design derives from MIT-licensed work: obra/superpowers © 2025 Jesse Vincent; mattpocock/skills © 2026 Matt Pocock. Full notices land in `docs/ATTRIBUTIONS.md` alongside the per-skill source mapping in §4.
