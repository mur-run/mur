# Smart Project Search Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `mur project status` a versioned JSON output and rewrite the `mur-project-search` builtin skill so an agent routes each code search to semantic search, `rg`, or a hybrid of both based on query intent and real index/worktree state.

**Architecture:** Two independent deliverables. (1) A thin CLI addition: `mur project status --json` serializes the existing `ProjectStatusInfo` behind a `schema_version` wrapper, leaving the human text output byte-identical. (2) A skill rewrite: the routing decision lives in the model, the shell contributes only deterministic facts (`mur project status --json` for index state, `git status --porcelain=v1 -z` for changed paths). No bash wrapper, no result merging in shell, no new helper binary.

**Tech Stack:** Rust 2024 (clap derive, serde/serde_json), MUR builtin skill YAML (`mur-core/src/skills/`), `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-14-smart-project-search-routing-design.md`

## Global Constraints

- **Branch off `main`.** The repo is currently on `test/all-four-fixes`; create `feat/project-status-json-routing` from `main` before Task 1. Re-check the branch as its own tool call immediately before every `git commit` (a `git branch --show-current && git commit …` chain is not a check).
- **Build env — export these before ANY cargo command, in every shell:**
  ```bash
  export ORT_STRATEGY=download
  export MUR_WEB_DIST="$HOME/Projects/mur-web/dist"
  ```
  Without `MUR_WEB_DIST` pointing at an existing built dir, `mur-core` fails to compile with `no associated function 'get' found for struct 'WebAssets'` — a red herring unrelated to this work.
- **Tests run under `cargo nextest run`, never `cargo test`** (~7 mur-core tests need process isolation). Scope every run with `-p mur-core <filter>` so the pre-existing `bin/mur` clap stack-overflow SIGABRTs stay out of the way.
- **JSON schema version is `1`** and the payload keys are exactly the existing `ProjectStatusInfo` fields plus `schema_version`. Do not rename, drop, or reorder existing fields.
- **Human text output of `mur project status` must not change.** No new lines, no reordering, no wording edits.
- **Skill disclosure budgets** (enforced by `new_builtin_skills_parse_and_respect_disclosure_budgets`): `description` ≤ 120 chars, `abstract` ≤ 50 words, `context` body ≤ 150 lines.
- **No docs task.** `README.md` and the docs site do not document `mur project` at all; adding `--json` to an undocumented command creates no doc entry point. Do not open a docs PR for this change.
- **No status helper script/binary.** The spec marks it optional ("若需要"); two one-liners in the skill cover it. Add a helper only if a future skill needs the same facts and the one-liners start drifting between copies.

---

### Task 1: `mur project status --json`

**Files:**
- Modify: `mur-core/src/cmd/project.rs` (add wrapper type + `status_json()` near `ProjectStatusInfo` at :44-66; change `cmd_project_status` at :452)
- Modify: `mur-core/src/cli/actions.rs:1344-1348` (the `ProjectAction::Status` variant)
- Modify: `mur-core/src/dispatch.rs:1626` (the `ProjectAction::Status` arm)
- Test: `mur-core/src/cmd/project.rs` (new `#[cfg(test)] mod status_json_tests` at end of file)

**Interfaces:**
- Consumes: existing `ProjectStatusInfo`, `IndexProgressInfo` (both `pub`, all fields `pub`, both `serde::Serialize`).
- Produces:
  - `pub const PROJECT_STATUS_SCHEMA_VERSION: u32 = 1;`
  - `pub struct ProjectStatusJson { pub schema_version: u32, pub info: ProjectStatusInfo }` (`info` is `#[serde(flatten)]`)
  - `pub fn status_json(info: &ProjectStatusInfo) -> anyhow::Result<String>` — pretty-printed JSON
  - `pub fn cmd_project_status(path: Option<String>, json: bool) -> anyhow::Result<()>` (signature change: second parameter added)

- [ ] **Step 1: Write the failing tests**

Append to the end of `mur-core/src/cmd/project.rs`:

```rust
#[cfg(test)]
mod status_json_tests {
    use super::*;

    fn base() -> ProjectStatusInfo {
        ProjectStatusInfo {
            name: "mur".into(),
            path: "/tmp/mur".into(),
            indexed: false,
            chunks: None,
            last_indexed: None,
            indexing_in_progress: false,
            progress: None,
            stale_dims: None,
        }
    }

    fn parse(info: &ProjectStatusInfo) -> serde_json::Value {
        serde_json::from_str(&status_json(info).expect("serialize")).expect("valid JSON")
    }

    #[test]
    fn not_indexed_serializes_with_schema_version() {
        let v = parse(&base());
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["name"], "mur");
        assert_eq!(v["path"], "/tmp/mur");
        assert_eq!(v["indexed"], false);
        assert!(v["chunks"].is_null());
        assert_eq!(v["indexing_in_progress"], false);
        assert!(v["stale_dims"].is_null());
    }

    #[test]
    fn usable_index_reports_chunks_and_no_staleness() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(123),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["indexed"], true);
        assert_eq!(v["chunks"], 123);
        assert_eq!(v["indexing_in_progress"], false);
        assert!(v["stale_dims"].is_null());
    }

    #[test]
    fn indexing_in_progress_carries_progress_object() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(10),
            indexing_in_progress: true,
            progress: Some(IndexProgressInfo {
                done_chunks: 5,
                total_chunks: 20,
                pct: 25.0,
                errors: 1,
            }),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["indexing_in_progress"], true);
        assert_eq!(v["progress"]["done_chunks"], 5);
        assert_eq!(v["progress"]["total_chunks"], 20);
        assert_eq!(v["progress"]["errors"], 1);
    }

    #[test]
    fn stale_dims_serializes_as_recorded_then_configured() {
        let info = ProjectStatusInfo {
            indexed: true,
            chunks: Some(7),
            stale_dims: Some((768, 1024)),
            ..base()
        };
        let v = parse(&info);
        assert_eq!(v["stale_dims"][0], 768);
        assert_eq!(v["stale_dims"][1], 1024);
    }

    /// The JSON keys are a contract for skills and agents: a rename here
    /// silently breaks every consumer's `usable` check. Pin the whole key set.
    #[test]
    fn json_key_set_is_exactly_the_documented_contract() {
        let v = parse(&base());
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "chunks",
                "indexed",
                "indexing_in_progress",
                "last_indexed",
                "name",
                "path",
                "progress",
                "schema_version",
                "stale_dims",
            ]
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo nextest run -p mur-core status_json_tests
```

Expected: compile error — `cannot find function 'status_json' in this scope`.

- [ ] **Step 3: Add the wrapper type and serializer**

In `mur-core/src/cmd/project.rs`, immediately after the `IndexProgressInfo` struct (currently ends at line 66):

```rust
/// Version of the `mur project status --json` payload. Bump ONLY on a
/// breaking field change — skills and agents gate their "is the index
/// usable" decision on this number, so a silent shape change is worse than
/// a loud version bump.
pub const PROJECT_STATUS_SCHEMA_VERSION: u32 = 1;

/// Machine-readable envelope for `mur project status --json`.
/// `info` is flattened so the JSON keys stay identical to
/// `ProjectStatusInfo`'s — the version is additive, not a nesting change.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProjectStatusJson {
    pub schema_version: u32,
    #[serde(flatten)]
    pub info: ProjectStatusInfo,
}

/// Serialize one project's status as versioned, pretty-printed JSON.
// ponytail: clones a small struct instead of threading a lifetime through
// the wrapper; borrow it if this ever lands in a hot loop.
pub fn status_json(info: &ProjectStatusInfo) -> Result<String> {
    let payload = ProjectStatusJson {
        schema_version: PROJECT_STATUS_SCHEMA_VERSION,
        info: info.clone(),
    };
    Ok(serde_json::to_string_pretty(&payload)?)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo nextest run -p mur-core status_json_tests
```

Expected: 5 passed.

- [ ] **Step 5: Add the `--json` flag to the CLI**

In `mur-core/src/cli/actions.rs`, replace the `Status` variant (lines 1344-1348):

```rust
    /// Show indexing status for a project
    Status {
        #[arg(long)]
        path: Option<String>,
        /// Machine-readable JSON (schema_version 1) instead of the text summary
        #[arg(long)]
        json: bool,
    },
```

- [ ] **Step 6: Wire the flag through dispatch and the command**

In `mur-core/src/dispatch.rs`, replace line 1626:

```rust
            ProjectAction::Status { path, json } => cmd::project::cmd_project_status(path, json)?,
```

In `mur-core/src/cmd/project.rs`, replace the first three lines of `cmd_project_status` (line 452 onward):

```rust
pub fn cmd_project_status(path: Option<String>, json: bool) -> Result<()> {
    let info = do_project_status(path.as_deref())?;

    if json {
        println!("{}", status_json(&info)?);
        return Ok(());
    }

    println!("Project: {}", info.name);
```

Leave every line after that untouched — the text branch must stay byte-identical.

- [ ] **Step 7: Verify the whole crate still compiles and nothing else called the old signature**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo check --workspace --all-targets
```

Expected: no errors. A workspace-wide check is required, not `-p mur-core`: `cmd_project_status` is `pub`, and a single-crate check does not catch a downstream caller.

- [ ] **Step 8: Verify the real binary end-to-end**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo run -p mur-core --bin mur -- project status --path "$PWD" --json
```

Expected: pretty JSON whose first key is `"schema_version": 1`. Then confirm the text path is unchanged:

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo run -p mur-core --bin mur -- project status --path "$PWD"
```

Expected: the same `Project: … / Path: … / Indexed: …` block as before this task.

- [ ] **Step 9: Lint**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check
```

Expected: both clean. Read the exit code — a clippy run without `-D warnings`, or one whose output you only grepped, reports clean on a crate that is not.

- [ ] **Step 10: Commit**

```bash
git branch --show-current
```

Then, only if it prints `feat/project-status-json-routing`:

```bash
git add mur-core/src/cmd/project.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "$(cat <<'EOF'
feat(project): add versioned JSON output to `mur project status`

Agents routing between semantic search and rg need index state as data,
not as human text they scrape. `--json` emits the existing
ProjectStatusInfo fields behind schema_version 1; the text output is
untouched.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Rewrite the `mur-project-search` skill

**Files:**
- Modify: `mur-core/src/skills/mur_project_search.yaml` (full rewrite of `content:`)
- Modify: `mur-core/src/cmd/sync_cmd.rs` (add a case to the budget list in `builtin_skill_tests`, ~line 1871-2075; add one new test to the same module)

**Interfaces:**
- Consumes: `mur project status --path <root> --json` from Task 1 — specifically the keys `indexed`, `indexing_in_progress`, `stale_dims`.
- Produces: the skill body text that Task 3's routing eval is run against.

- [ ] **Step 1: Write the failing tests**

In `mur-core/src/cmd/sync_cmd.rs`, add this case to the `cases` array inside `new_builtin_skills_parse_and_respect_disclosure_budgets` (put it next to the other `mur-project-*` entries; `false` = default `Indexed` visibility, which this skill's YAML has by omission):

```rust
            (
                "mur-project-search",
                include_str!("../skills/mur_project_search.yaml"),
                false,
            ),
```

Then add this test to the same `builtin_skill_tests` module, after `every_builtin_skill_yaml_parses`:

```rust
    /// The routing contract IS this skill: delete a rule and the agent
    /// silently loses a branch — no parse error, no failing build, just a
    /// worse search forever after.
    ///
    /// What this proves: the rules survive an edit and the file still parses.
    /// What it does NOT prove: that a model obeys them. That check is the
    /// fresh-context routing eval in
    /// docs/superpowers/plans/2026-09-14-smart-project-search-routing.md.
    #[test]
    fn project_search_skill_carries_the_routing_contract() {
        let m =
            mur_common::skill::parse_canonical(include_str!("../skills/mur_project_search.yaml"))
                .expect("mur-project-search must parse");
        let body = m.content.context.clone().unwrap_or_default();
        for needle in [
            "mur project status",
            "--json",
            "indexing_in_progress",
            "stale_dims",
            "git status --porcelain=v1 -z",
            "changed paths",
            "--all",
            "--project",
        ] {
            assert!(
                body.contains(needle),
                "routing rule missing from mur-project-search: {needle}"
            );
        }
        // The old body claimed the default scope was every indexed project.
        // The code defaults to the current directory's project; a skill that
        // says otherwise teaches agents to misread their own results.
        assert!(
            !body.contains("searches across ALL indexed projects"),
            "stale scope claim still present in mur-project-search"
        );
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo nextest run -p mur-core builtin_skill_tests
```

Expected: `project_search_skill_carries_the_routing_contract` FAILS with `routing rule missing from mur-project-search: mur project status --json`-style output (the current body has none of the new rules), and the `!contains` assertion also fails on the stale scope claim.

- [ ] **Step 3: Rewrite the skill YAML**

Replace the whole of `mur-core/src/skills/mur_project_search.yaml` with:

```yaml
name: mur-project-search
version: 0.2.0
publisher: human:mur
description: "Search project code by meaning. Use for concept/intent queries; use grep for exact strings and exhaustive matches."
category: context
hosts: [all]
content:
  abstract: |
    Route by query type: exact symbols, strings and exhaustive sweeps go to
    `rg`; intent questions go to `mur project search` (or the
    mur_project_search MCP tool) when `mur project status --json` says the
    index is usable, plus `rg` over git-changed paths when the tree is dirty.
  context: |
    # mur-project-search — route a code search to the right tool

    Semantic search (hybrid vector + BM25) answers *intent* questions from an
    indexed snapshot. `rg` is exact, exhaustive, and always reflects the
    working tree right now. Route per query; neither tool is the default.

    ## 1. Classify the query first
    - **exact** — a known symbol, string, import, config key, error message.
    - **exhaustive** — every occurrence: rename, find all callers, dead-code
      sweep, "is this still used anywhere".
    - **intent** — a concept or behavior: "where is retry handled", "how does
      auth work", "which file scores decay".

    exact or exhaustive → whole-project `rg`, immediately. Skip the rest of
    this skill: index state cannot change the answer to a question `rg`
    answers exactly and completely.

    ## 2. For intent queries, read index state as JSON
    ```bash
    mur project status --path "$(git rev-parse --show-toplevel 2>/dev/null || pwd)" --json
    ```
    Never scrape the human text output. The index is **usable** when
    `indexed == true` AND `indexing_in_progress == false` AND
    `stale_dims == null`. If the command fails, treat the index as unusable
    and say so — do not wait for indexing, do not start an index.

    ## 3. Ask git what the snapshot cannot know
    ```bash
    git status --porcelain=v1 -z | tr '\0' '\n'
    ```
    Entries are `XY <path>`; a rename emits the new path and then the old one.
    **changed paths** = modified + staged + untracked + renamed-to.
    Deleted paths and renamed-from paths are NOT searched — they are the list
    of paths whose semantic hits you must distrust.
    A git failure means "not a Git project", not "search failed".

    ## 4. Route
    | Query | Index usable | Git | Do |
    |---|---|---|---|
    | exact / exhaustive | any | any | whole-project `rg` |
    | intent | yes | clean | `mur_project_search` |
    | intent | yes | dirty | `mur_project_search` + `rg` over changed paths |
    | intent | yes | non-Git | `mur_project_search` |
    | intent | no | any | whole-project `rg` |

    A failing semantic call routes like an unusable index: whole-project `rg`,
    and report why.

    Hybrid is two searches and one answer: run the semantic call, then
    `rg -n "<term>" -- <changed paths that still exist>`. One unrelated dirty
    file never cancels the semantic half — that is the whole point of asking
    git for paths instead of asking it whether the tree is clean.

    ## 5. Scope
    `mur project search` and the `mur_project_search` MCP tool search **the
    current directory's project** by default. Pass `--project <name>` for a
    different one. Pass `--all` only when the user actually asked to look
    across projects. `--limit <n>` widens or narrows the ranked list.

    ## Hard rules (correctness)
    - A semantic hit in a deleted or renamed-from path is stale. Do not report
      it as current; check the working tree first.
    - Zero semantic results is not evidence of absence — it is ranked top-k
      over a snapshot. Confirm with `rg` before saying code does not exist.
    - Label each finding by origin: indexed snapshot or working tree. Open the
      file when the answer has to be current.
    - If a changed path no longer exists when `rg` runs, skip that path and
      note it; never fail the whole search on one stale path.
    - Code you edited this session and have not committed is not in the index.
      The post-commit hook refreshes it — see the mur-project-index skill.
tags: [mur, project, search, grep, builtin]
triggers:
  - type: keyword
    pattern: "(where is the code|how does .{0,30} work|which file (handles|is responsible)|find the (logic|code) (that|responsible)|semantic.{0,8}search|search the (codebase|project) for)"
  - type: manual
priority: normal
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo nextest run -p mur-core builtin_skill_tests sync_skill_tests
```

Expected: all pass, including `new_builtin_skills_parse_and_respect_disclosure_budgets` (budgets: description ≤ 120 chars, abstract ≤ 50 words, context ≤ 150 lines), `every_builtin_skill_yaml_parses`, `installs_project_search_skill`, and the new `project_search_skill_carries_the_routing_contract`.

If the budget assertion fails, cut lines from the "Hard rules" section — not from the routing table.

- [ ] **Step 5: Verify the skill installs and parses through the real path**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo run -p mur-core --bin mur -- sync
sed -n '1,20p' ~/.mur/skills/mur-project-search/skill.yaml
```

Expected: `mur sync` reports no WARN about `mur-project-search`, and the installed file shows `version: 0.2.0`.

- [ ] **Step 6: Lint**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo clippy -p mur-core --all-targets -- -D warnings
cargo fmt --check
```

Expected: both clean.

- [ ] **Step 7: Commit**

```bash
git branch --show-current
```

Then, only if it prints `feat/project-status-json-routing`:

```bash
git add mur-core/src/skills/mur_project_search.yaml mur-core/src/cmd/sync_cmd.rs
git commit -m "$(cat <<'EOF'
feat(skills): route project search by intent and real index state

The skill claimed the default scope was every indexed project (it is the
current directory's) and used `git status` cleanliness as a single
on/off switch, so one unrelated edit threw away usable semantic recall.

It now classifies the query first, reads index state from
`mur project status --json`, and in a dirty tree runs semantic search plus
`rg` over exactly the changed paths.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Fresh-context routing eval

**Files:**
- Create: `docs/superpowers/validation/2026-09-14-project-search-routing-eval.md`

**Interfaces:**
- Consumes: the skill body written in Task 2 and the `--json` output from Task 1.
- Produces: a recorded pass/fail table. This is a **model-behavior check, not a CI gate** — no test target, no build hook. It exists because Task 2's test proves only that the rules are present in the file.

- [ ] **Step 1: Run the five scenarios against fresh subagents**

For each row below, dispatch ONE fresh general-purpose subagent (Agent tool, `subagent_type: "general-purpose"`). Give it exactly this prompt, with `<SKILL BODY>` replaced by the verbatim `content.context` block from `mur-core/src/skills/mur_project_search.yaml` and `<SCENARIO>` replaced by the scenario text:

```
You are answering a code-search question in a repository. These are your
search instructions:

<SKILL BODY>

Situation: <SCENARIO>

Answer with exactly two lines:
TOOL: <one of: rg | semantic | hybrid>
WHY: <one sentence>

Do not run any tools. Do not explain further.
```

| # | Scenario | Expected `TOOL` |
|---|---|---|
| 1 | "The user asks: where is the logic that decides when to retry a failed request? `mur project status --json` returned `{\"schema_version\":1,\"indexed\":true,\"indexing_in_progress\":false,\"stale_dims\":null}`. `git status --porcelain=v1 -z` returned nothing." | `semantic` |
| 2 | "The user asks: find every caller of `resolve_secret` so I can rename it. The index is usable and the tree is clean." | `rg` |
| 3 | "The user asks: how does the channel signing flow work? Index status is `indexed:true, indexing_in_progress:false, stale_dims:null`. `git status --porcelain=v1 -z` lists ` M README.md` and `?? notes.txt`." | `hybrid` |
| 4 | "The user asks: which file is responsible for decay scoring? `mur project status --json` reports `indexed:true, indexing_in_progress:false, stale_dims:null`. `git rev-parse --show-toplevel` failed: this directory is not a git repository." | `semantic` |
| 5 | "The user asks: where is the code that renders the status panel? `mur project status --json` reports `indexed:true, indexing_in_progress:true` with progress 40/200." | `rg` |

Scenario 6 (stale dims), same shape as 5 but `indexing_in_progress:false, stale_dims:[768,1024]` → expected `rg`.

- [ ] **Step 2: Record the results verbatim**

Write `docs/superpowers/validation/2026-09-14-project-search-routing-eval.md` containing: the date, the skill version tested (`0.2.0`), a table of scenario # / expected / actual `TOOL` / the agent's `WHY` line quoted verbatim, and a one-line verdict. Paste the actual answers — do not summarize them into "passed".

- [ ] **Step 3: Fix the skill if a scenario misroutes**

If any scenario returns the wrong `TOOL`, the skill text is at fault, not the eval. Edit `mur-core/src/skills/mur_project_search.yaml` to make that rule unmissable (usually: move it earlier, or state the branch as an explicit row), re-run Task 2 Step 4, then re-run the failing scenario with a NEW subagent — never the same one, it has already seen the answer. Record both attempts in the eval file.

- [ ] **Step 4: Commit**

```bash
git branch --show-current
```

Then, only if it prints `feat/project-status-json-routing`:

```bash
git add docs/superpowers/validation/2026-09-14-project-search-routing-eval.md mur-core/src/skills/mur_project_search.yaml
git commit -m "$(cat <<'EOF'
test(skills): record fresh-context routing eval for mur-project-search

Six scenarios covering each row of the routing table, answered by fresh
subagents given only the skill body. The automated test proves the rules
are in the file; this is the only evidence a reader actually follows them.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Full verification and branch finish

**Files:** none modified.

- [ ] **Step 1: Run the scoped test suite**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo nextest run -p mur-core status_json_tests builtin_skill_tests sync_skill_tests dev_skill_trigger_tests
```

Expected: all pass. Print the full output; a summary line you did not read is not evidence.

- [ ] **Step 2: Run the workspace gate**

```bash
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" RUST_MIN_STACK=33554432 \
  cargo nextest run -p mur-core
```

Expected: clippy and fmt clean. The full `-p mur-core` run needs `RUST_MIN_STACK=33554432` — without it ~7 `bin/mur` clap-parse tests SIGABRT on a pre-existing debug stack overflow unrelated to this change.

- [ ] **Step 3: Check the workspace-excluded Hub still compiles**

`cmd_project_status`'s signature changed and `mur-hub-gui` is excluded from the workspace, so `cargo check --workspace` does not cover it:

```bash
rg -n "cmd_project_status|ProjectAction::Status" mur-hub-gui mur-agent-gui
```

Expected: no matches (neither GUI calls it). If there are matches, build that crate via its own manifest before finishing.

- [ ] **Step 4: Finish the branch**

Use the `superpowers:finishing-a-development-branch` skill. The PR body must state which checks were run with their actual output, and must link both the spec and this plan.

---

## Out of scope (deliberate)

- No bash wrapper from query to results, and no `--auto` mode: intent classification cannot be a correctness contract in shell. A future human CLI should ship explicit `--intent` / `--exact` / `--all-callers` modes instead.
- No merging, re-ranking, or score comparison between semantic hits and `rg` hits. The agent reads both lists and labels each finding's origin.
- No index creation or rebuild from the routing path. The router observes and falls back; `mur project index` stays an explicit action.
- No git dirty-path fields in `ProjectStatusInfo`. Git is an optional VCS capability; keeping it out is what lets status stay a correct index API in a non-Git directory.
