# mur-project-search routing eval — 2026-09-14

Skill version under test: **0.2.0** (`mur-core/src/skills/mur_project_search.yaml`
at commit `fe5cf6f7`, context body 80 lines).

## What this is, and what it is not

The unit test `project_search_skill_carries_the_routing_contract` asserts that
the routing rules are present in the YAML. That is a regression guard against
someone deleting a rule; it proves nothing about whether a reader follows them,
and it would still pass if two rows of the table were swapped.

This eval is the other half. Each scenario was given to a **fresh subagent**
whose entire context was the skill's `context` body — no plan, no spec, no
conversation history, no repository access beyond that one file — plus a
situation and the instruction to answer with a `TOOL:` line and a one-sentence
`WHY:`. Six scenarios cover all five rows of the routing table.

Model: **Sonnet**, passed as an explicit per-dispatch override (the plan's text
omitted it, so without the override these would have run on the session default
instead). It is the tier that actually executes these skills; a cheaper model
failing would not separate "the text is ambiguous" from "the model is weak", so
the eval is run at the tier the result has to hold for.

This is a model-behavior check, not a CI gate: it has no test target and runs
nowhere automatically. Re-run it by hand when the routing text changes.

## Results

All six matched the expected route. `TOOL:` and `WHY:` lines are quoted exactly
as the subagents produced them.

| # | Scenario | Table row | Expected | Actual |
|---|---|---|---|---|
| 1 | intent, index usable, clean tree | intent/yes/clean | semantic | ✅ semantic |
| 2 | "find every caller of `resolve_secret` so I can rename it" | exhaustive | rg | ✅ rg |
| 3 | intent, index usable, ` M README.md` + `?? notes.txt` | intent/yes/dirty | hybrid | ✅ hybrid |
| 4 | intent, index usable, `git rev-parse` exit 128 (not a repo) | intent/yes/non-Git | semantic | ✅ semantic |
| 5 | intent, `indexing_in_progress: true` (40/200) | intent/no | rg | ✅ rg |
| 6 | intent, `stale_dims: [768, 1024]` | intent/no | rg | ✅ rg |

**1.** `TOOL: semantic` — "This is an intent query ("where is retry logic"), the
index is usable (indexed, not in-progress, stale_dims null), and git is clean,
so per the routing table `mur_project_search` alone applies with no changed
paths to supplement with `rg`."

**2.** `TOOL: rg` — "Finding every caller to rename is an exhaustive query,
which the skill routes directly to whole-project `rg` regardless of index
state."

**3.** `TOOL: hybrid` — "The index is usable (indexed, not in progress,
stale_dims null) but the working tree is dirty, so this is an intent query
routed to semantic + rg over the changed paths (README.md, notes.txt)."

**4.** `TOOL: semantic` — "The query is an intent question ("which file scores
decay") and the index is usable (indexed, not in-progress, no stale dims),
while git failed with exit 128 meaning this isn't a Git project, so the routing
table's "intent / index usable / non-Git" row applies, calling for
`mur_project_search` alone rather than the hybrid rg-augmented path."

**5.** `TOOL: rg` — "indexing_in_progress is true, so the index is unusable and
the query routes to whole-project rg regardless of the clean git tree."

**6.** `TOOL: rg` — "stale_dims is [768,1024] (not null), so the index fails the
usable check and an intent query must fall back to whole-project rg."

## Verdict

Six of six correct, no re-runs and no edits to the skill needed.

Worth noting in the reasons, not just the verdicts: scenarios 4, 5 and 6 each
names the specific condition that decided the route (`exit 128` → non-Git rather
than "search failed"; `indexing_in_progress`; `stale_dims` non-null). Those are
exactly the three cases the previous version of this skill got wrong or did not
cover — it treated git failure as failure, and offered no rule for an index
that exists but cannot be trusted.

## Limits of this evidence

- Six samples, one run each. A borderline-ambiguous rule could pass here and
  still misroute at a different temperature or in a longer conversation where
  the skill body competes with other context.
- Each scenario handed the subagent the command output rather than making it
  run the commands, so this tests the routing decision, not the agent's ability
  to produce correct `mur project status --json` / `git status` invocations.
  The shell in the skill was verified separately by the Task 2 reviewer, which
  reproduced `git status --porcelain=v1 -z` rename ordering and root-relative
  paths in a scratch repository.

## Appendix — the situations, verbatim

Each subagent received the skill body file path plus exactly one of these, and
the instruction to answer with a `TOOL:` line and a one-sentence `WHY:`. They
are recorded because a reason quoted above can only be judged against the input
that produced it — scenario 4's "exit 128", for instance, was given, not
inferred by the model.

1. The user asks "where is the logic that decides when to retry a failed
   request?" You already ran `mur project status --path "$root" --json` and it
   returned `{"schema_version":1,"indexed":true,"chunks":4210,"last_indexed":null,"indexing_in_progress":false,"progress":null,"stale_dims":null}`.
   You already ran `git status --porcelain=v1 -z` and it produced no output at all.
2. The user asks "find every caller of `resolve_secret` so I can rename it."
   The project's index is fully usable and the git working tree is clean.
3. The user asks "how does the channel signing flow work?" … `{"schema_version":1,"indexed":true,…,"indexing_in_progress":false,"progress":null,"stale_dims":null}`.
   You already ran `git status --porcelain=v1 -z | tr '\0' '\n'` and it printed
   two lines: ` M README.md` and `?? notes.txt`.
4. The user asks "which file is responsible for decay scoring?" … `"indexed":true,…,"stale_dims":null`.
   `git rev-parse --show-toplevel` failed with exit 128 — this directory is not
   inside a git repository at all.
5. The user asks "where is the code that renders the status panel?" …
   `"indexed":true,…,"indexing_in_progress":true,"progress":{"done_chunks":40,"total_chunks":200,"pct":20.0,"errors":0},"stale_dims":null}`.
   The git working tree is clean.
6. The user asks "how does the companion decide when to nudge?" …
   `"indexed":true,…,"indexing_in_progress":false,"progress":null,"stale_dims":[768,1024]}`.
   The git working tree is clean.
