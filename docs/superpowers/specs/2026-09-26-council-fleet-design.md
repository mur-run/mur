# Council fleet — senior advisory panel (design)

Status: approved design, v1 not yet built
Research: 3 deep-research reports + 3 cross-verifications, kept as local run
artifacts on the author's machine (not committed). This spec is self-contained;
the code citations below are the evidence it relies on, and the
research findings it cites by ID are summarized in the appendix.

## Goal

A read-only fleet that turns one design question into one recommendation
document: independent proposals, one anonymized peer-review round, a
vote-counted synthesis, and a preserved minority opinion. It never edits a
file; its only output is the recommendation.

## Members

| Agent | Role | Brief |
|---|---|---|
| `council-architect` | Architecture advisor | Long-term boundaries, abstractions, trade-offs |
| `council-implementer` | Implementation advisor | Feasibility, cost, risk, concrete steps |
| `council-challenger` | Challenge advisor | Failure modes; counters sycophancy (A2) |
| `tech-writer` | Clerk | Builds the evidence pack: verbatim excerpts + `file:line` only, no summaries |
| `orchestrator` (router) | Synthesizer | Plans rounds, counts votes, lists minority. Does not judge |

Three advisors is the smallest panel that yields a majority (A1).

### Challenger rule

A2 supports an assigned devil's-advocate role, but "mandatory dissent" has no
located primary source. So dissent must be grounded: every objection cites a
`file:line` from the evidence pack or states one concrete failure scenario.
Ungrounded objections are dropped at synthesis.

### Synthesizer rule

- A1: the gain comes mostly from majority vote, not debate. The synthesizer
  tallies positions and lists the minority; it does not re-adjudicate.
- A5: `Self-preference bias — GPT-4 +10%, Claude +25%`,
  `Position bias swing — 10–15 winrate points`. Peer review is anonymized
  (A/B/C) **and** each reviewer sees proposals in its own shuffled order.

## Models

| Version | Advisors | Effort | Note |
|---|---|---|---|
| v1 | all `claude_opus` (claude-opus-5-5) | `max` | A3-4 is contested (Self-MoA); diversity comes from roles |
| v2 | challenger moves to a GPT-family model | `max` | Blocked on gap 5 (below); also dilutes A5 self-preference |

Clerk and synthesizer do not need frontier reasoning; they keep their current models.

### Gap 5 (separate PR, not part of council)

`mur-common/src/llm.rs:393`:
```rust
const REASONING_FAMILIES: &[&str] = &["gpt-5", "o1", "o3", "o4"];
```
- gpt-5 `max` → `high` is intentional (`never fail the default path, lose a little depth instead`).
- gpt-6 is missing from the list, so no effort is sent at all. Fix: add the
  prefix + a test. v2 depends on it.

## Flow

1. **Pack.** Clerk builds one shared evidence pack from the question. Key
   excerpts at the start and end (Lost in the Middle). Verbatim + `file:line` only.
2. **Round 1 — independent.** Each advisor proposes without seeing the others.
   Each ends with an optional `DOC_REQUEST` list.
3. **Top-up (once).** Clerk answers all `DOC_REQUEST`s in a single pass. No
   second top-up (MAST: step repetition 15.7%).
4. **Round 2 — anonymized review.** Each advisor reviews the others as A/B/C,
   in a per-reviewer shuffled order. One round; at most two (A4: gains peak at
   2–3 rounds). Early stop when all three already agree.
5. **Synthesis.** Tally, recommendation, separate minority section (or an
   explicit "unanimous").

Termination is rule-based, not judged (MAST: unaware of termination 12.4%).

## Permissions

- Advisors and clerk: tools `read_file` + `recall` only. No write, no bash
  for advisors.
- A6-4: `Multi-agent works when writes stay single-threaded` — here nobody writes.
- Prerequisite: `mur agent perm tool-allow tech-writer read_file` before the
  fleet is created; today tech-writer only allows `bash`.
- Not added to `skillsmith` (`pm / qa / rustsmith / repomanager`, a writing
  fleet): mixing a read-only panel into it would couple tool allowlists.

## Routing

R1 (round 3 decision): the fleet executor relays asks; the orchestrator only
plans. Relay rules live in `config.yaml` `fleet_ask`. Round-3 items
(`EventKind::Ask` / `AskReply`, asker self-signs, replier computes the
intersection) are a dependency of the full flow, not part of this spec.

## Limits

`limits:` must set all three: `deadline`, `stuck`, `cost_usd`. Autorun stays
off (`MUR_FLEET_AUTORUN` unset); the council runs attended only.

## Acceptance

| # | Criterion | Check |
|---|---|---|
| 1 | All three produce round-1 proposals blind | Channel order: all three proposals precede any cross-delivery |
| 2 | Review is anonymized | Round-2 inputs carry no member names, only A/B/C, in differing orders |
| 3 | Terminates on its own | ≤ 2 review rounds, ended by rule |
| 4 | Minority preserved | Separate minority section, or explicit "unanimous" |
| 5 | Clerk is extractive | Every excerpt greps back verbatim to its `file:line` |
| 6 | Read-only | Settlement shows 0 files changed |
| 7 | Bounded | `deadline`, `stuck`, `cost_usd` all set |
| 8 | Reaches known answers | Two historical questions (below) reproduce the decision or justify the difference |

### Criterion 8 pilot questions

Both answers were decided in round 3. The pack **must** contain the evidence
that drove each decision; otherwise a failure measures pack coverage, not the
process. Verified on `origin/main` (951c64f6 and later):

**Q1 — R1 routing (why the executor relays, not the orchestrator via config):**
- `mur-common/src/paths.rs:17-22` — `fleets/` is "Configuration, not run
  state — deliberately absent from [`RUN_STATE_DIRS`]".
- `mur-common/src/paths.rs:48` — `pub const RUN_STATE_DIRS`.
- `mur-agent-runtime/src/sandbox/policy.rs:414-420` — `fleet_run_write_dirs`
  grants; `policy.rs:1002-1008` carves `fleet-state/` per allowlisted fleet.
- `mur-agent-runtime/src/tools/fleet_run.rs:12` — "the gate lives in
  `~/.mur/config.yaml`" (out-of-model; no agent can write it).

**Q2 — Ask is self-signed by the asker:**
- `mur-agent-runtime/src/communication_policy.rs:1` — calls `accepts_from`
  the "authoritative security boundary"; `accepts_from_allows` is called only
  from `mur-agent-runtime/tests/comm_policy.rs`.
- `mur-agent-runtime/src/transport/unix_socket.rs:130` —
  `let _ = peer; // passed to auth / communication_policy via request context in Task 22`
  (peer credentials dropped).
- `mur-agent-runtime/src/sandbox/policy.rs:23` — `profile.yaml` in
  `SELF_PROTECTED_AGENT_FILES`, the write-protected list (applied to the file
  tools at `tools/fs_policy/mod.rs:257`).

Known wrong answers the pack must be able to rule out: B1, and a
`read_scope` request parameter.

## Build order

1. `mur agent perm tool-allow tech-writer read_file`
2. Create `council-architect`, `council-implementer`, `council-challenger`
   (`claude_opus`, `effort: max`, tools `read_file` + `recall`).
3. `mur fleet create council --router orchestrator --members …`
4. `mur fleet limits council --deadline … --stuck … --cost-usd …`
5. Pilot Q1 and Q2; score against the acceptance table.

## Appendix: research findings

Summary of the research this spec cites by ID (A1–A7). Quotes are as the
research workers recorded them; the verification column is from the three
cross-verification passes (correctness, recency, source independence).

| ID | Finding used by this design | Source | Verification |
|---|---|---|---|
| A1 | "Majority Voting alone accounts for most of the performance gains typically attributed to MAD" | Choi et al., NeurIPS 2025 — arxiv.org/abs/2508.17536 | Confirmed |
| A1 | Homogeneous debate costs 2.1–3.4× the tokens of isolated self-correction, at equal or lower accuracy | arxiv.org/abs/2605.00914 | Confirmed |
| A2 | "sycophancy is a core failure mode that amplifies disagreement collapse … in multi-agent debates" | Yao et al. 2025 — arxiv.org/abs/2509.23055 | Confirmed |
| A2 | "the adaptive break of debate and the modest level of 'tit for tat' state are required for MAD to obtain good performance" (assigned opposing roles) | Liang et al., EMNLP 2024 — aclanthology.org/2024.emnlp-main.992 | Confirmed |
| A2 | Anonymous Delphi-style rounds preserve answer diversity | DeLLMphi — openreview.org/forum?id=W7a4UjLk1I | Confirmed |
| A2 | Mandatory dissent / minority report with measured effect | — | **Not found** (hence the grounded-dissent rule) |
| A3 | Heterogeneous mixtures beat a single model (MoA) | Wang et al., ICLR 2025 — arxiv.org/abs/2406.04692 | Confirmed |
| A3 | **Contested:** a single strong model's own samples (Self-MoA) "outperforms mixtures in most scenarios" | arxiv.org/abs/2502.00674 | Claim confirmed; effect sizes not extracted |
| A4 | "the maximum effect is achieved in the second or third round, after which further discussions can lead to the repetition of the same arguments" | systems-analysis.ru (literature synthesis) | Confirmed; recency pass qualifies it as diminishing returns rather than a hard plateau |
| A5 | Position bias: 10–15 winrate points of swing depending on slot order | Zheng et al. (MT-Bench), via futureagi.com; corroborated by Wang et al., ACL 2024 | Confirmed, secondary source |
| A5 | Self-preference bias: GPT-4 +10%, Claude +25% on own outputs | Zheng et al. — arxiv.org/abs/2306.05685 | **Qualified:** plausible, exact percentages not re-verified from the PDF |
| A6 | "multi-agent systems work best today when writes stay single-threaded and the additional agents contribute intelligence rather than actions" | Cognition — cognition.com/blog/multi-agents-working | Confirmed |
| A6 | "information positioned near the center of the context window is more likely to be overlooked" | Liu et al., TACL 2024 (Lost in the Middle) — aclanthology.org/2024.tacl-1.9 | Confirmed |
| A6 | Extractive compression (verbatim + citation) keeps precision that abstractive summaries lose | [RECOMP (Xu et al., ICLR 2024)](https://proceedings.iclr.cc/paper_files/paper/2024/file/bda88ed2892f5e61c9a9bf215c566913-Paper-Conference.pdf) | Confirmed |
| A7 | MAST: 14 failure modes over 1600+ traces, κ = 0.88; step repetition 15.7%, reasoning-action mismatch 13.2%, unaware of termination 12.4% | Cemri et al., NeurIPS 2025 — arxiv.org/abs/2503.13657 | Confirmed (spot-checked frequencies) |

Caveats:

- The source-independence pass rejected 14 of 24 corroboration pairs for
  A3/A5/A6: the "corroborating" link was often the same paper on another
  site. The claims still stand on their primary source, but most have one
  independent source, not two.
- The research also covered a second topic, design principles as agent
  skills (SOLID/DRY/KISS/YAGNI sources, published principle skills). It is
  unrelated to the B1 named under "Known wrong answers". None of it is
  used by this design, so it is not summarized here.
