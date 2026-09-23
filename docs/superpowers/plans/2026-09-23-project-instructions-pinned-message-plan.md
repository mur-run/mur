# Plan — project instructions as a pinned first user message

> Date: 2026-09-23
> Spec: `docs/superpowers/specs/2026-09-23-project-instructions-pinned-message-design.md`
> Baseline: PR #1475, branch `feat/runtime-project-instructions`, commit `69938cd9`,
> worktree `.worktrees/project-instructions`
> Execution: `mur-executing-plans` (single, sequential) — or `mur-delegate-dev`
> for T2/T3/T5 in parallel once T1 lands. TDD per `mur-tdd`: every step is
> RED → GREEN → REFACTOR; no production code without a failing test first.

---

## Goal

Move the `AGENTS.md` / `AGENT.md` / `CLAUDE.md` text out of the system prompt
and into a pinned first user message, with fair-share budgeting, typed refusal
handling, and send-time trimming — without touching any LLM adapter's
conversion code and without adding a `RichMessage` variant.

## Ticket map

Tickets touch **disjoint files** so they can be built in separate sessions or
worktrees and merged without conflicts. Edges are blocking edges.

| # | Title | Files (exclusive) | Blocked by |
|---|---|---|---|
| T1 | Split `project_instructions.rs` into a module dir (pure move) | `mur-agent-runtime/src/project_instructions/{mod,budget,decode,tests}.rs` | — |
| T2 | Typed `ReadRefusal` gate in `fs_policy` | `mur-agent-runtime/src/tools/fs_policy.rs` | — |
| T3 | Adapter position/merge tests (tests only) | `llm/anthropic/tests.rs`, `llm/openai/tests.rs`, `llm/ollama.rs` (test mod only) | — |
| T4 | Block renderer: XML format, escaping, decoding, shadowing, fair share, refusal rendering | `project_instructions/{mod,budget,decode,tests}.rs` | T1, T2 |
| T5 | Runner: `seed_history` pinned arg, send-time trim, cap, system paragraph, `opens_turn` doc | `mur-agent-runtime/src/task_runner.rs` | — (compiles against a temporary shim; see T5 step 0) |
| T6 | Integrate: wire `render(cwd, cap)` into both call sites, delete shim, end-to-end tests, docs | `task_runner.rs`, `supervisor_runner.rs`, `README.md`, docs site, product page | T4, T5 |

Frontier (startable now): **T1, T2, T3, T5**. T4 waits for T1+T2. T6 last.

Why T5 does not block on T4: T5 changes `seed_history`'s shape and adds the
trim, which are testable with a literal block string (spec §9 A5). It keeps
compiling against the baseline `render(cwd) -> Option<String>` by leaving the
`assemble_system_prompt` call in place behind a `// T6: remove` marker and
passing `pinned: None` at both call sites until T6. That keeps `task_runner.rs`
exclusive to T5 while T4 owns the renderer.

Ticket sizing: each is one fresh context window. T4 is the largest; if it
overruns, split at the marked seam (T4a format/escape/decode/shadow, T4b fair
share, T4c refusal rendering) — they are sequential within the same files.

---

## T1 — Split `project_instructions.rs` (pure move)

**Blocked by:** none.
**Files:** `mur-agent-runtime/src/project_instructions.rs` → directory
`mur-agent-runtime/src/project_instructions/` with `mod.rs`, `budget.rs`,
`decode.rs`, `tests.rs`. Pattern: `llm/anthropic/{mod,convert,tests}.rs`.

Why first: the module is 312 lines and T4 adds fair-share, decoding, escaping,
refusal mapping and ~12 tests. It would cross 800 lines (CLAUDE.md rule 4).
Rule 4 also says "pure code movement first; behavior changes in a separate PR" —
here: a separate commit within the branch.

Steps:
1. **Baseline green.** `cargo test -p mur-agent-runtime project_instructions`
   — all 8 pass. Record the count.
2. Move: `mod.rs` gets the module doc, `INSTRUCTION_FILES`,
   `MAX_PROJECT_INSTRUCTIONS_BYTES`, `HEADER`, `ProjectInstructions`, `render`,
   `discover`. `budget.rs` gets `clip` (as `pub(super)`). `decode.rs` is created
   empty except for a module doc (T4 fills it). `tests.rs` gets the test module
   verbatim (`#[cfg(test)] mod tests;` in `mod.rs`, `use super::*;` inside).
3. **Still green, same 8 tests, no new warnings.** `cargo clippy -p
   mur-agent-runtime --tests` clean.
4. Commit: `refactor(runtime): split project_instructions into a module dir (pure move)`.

Acceptance:
- [ ] Same 8 tests pass; `git diff --stat` shows only moves (no logic diff).
- [ ] No file in the directory exceeds 400 lines after the move (headroom for T4).

---

## T2 — Typed `ReadRefusal` in `fs_policy`

**Blocked by:** none.
**Files:** `mur-agent-runtime/src/tools/fs_policy.rs` only (707 lines today; the
addition is ~40 lines + tests — stays under 800. If it does not, split the test
module out first as a pure move.)

Contract (spec §5.1): a typed twin of `check_read_entitlement` that returns
`Result<(), ReadRefusal>` with variants `LaunchChain`, `DenyList`, `NoGrant`,
and `check_read_entitlement` implemented **by mapping from it**, so the string
gate and the typed gate cannot diverge (the "one function" promise at
`fs_policy.rs:308-312` is preserved: one decision function, two presentations).

Steps (each RED → GREEN):
1. RED: `read_refusal_launch_chain_wins_over_deny_and_grant` — a path the
   launch chain protects, also in `deny` and in `read`, → `LaunchChain`.
   Fails: function does not exist.
2. GREEN: introduce the enum and the typed function returning only
   `LaunchChain` for now (simplest thing).
3. RED: `read_refusal_deny_list_wins_over_grant` → `DenyList`.
4. GREEN.
5. RED: `read_refusal_no_grant_when_outside_every_root` → `NoGrant`;
   `read_refusal_ok_under_read_or_write_grant` → `Ok(())` for both lists.
6. GREEN.
7. RED: `check_read_entitlement_strings_are_unchanged` — for each variant, the
   string gate's `Display` output equals the literal it produces today (copy the
   three literals from `fs_policy.rs:322-340` as the expected values, from the
   baseline commit, not from the new code).
8. GREEN: reimplement `check_read_entitlement` as `typed(...).map_err(|r| …)`.
9. REFACTOR: the three format strings live next to the enum; existing
   `read_file` tests still pass.
10. Commit: `feat(runtime): typed ReadRefusal behind check_read_entitlement`.

Acceptance:
- [ ] Spec §7.2 both boxes.
- [ ] `ReadRefusal` derives `Debug, Clone, Copy, PartialEq, Eq`; is
      `pub(crate)`; has **no** `Display` impl carrying paths (the block must
      never embed a path via error text).
- [ ] `cargo test -p mur-agent-runtime fs_policy` green; `read_file` tests
      untouched and green.

---

## T3 — Adapter position/merge tests (tests only)

**Blocked by:** none (the behaviour under test is the *existing* adapter code;
the tests document what the pinned message will do there).
**Files:** `mur-agent-runtime/src/llm/anthropic/tests.rs`,
`mur-agent-runtime/src/llm/openai/tests.rs`, `mur-agent-runtime/src/llm/ollama.rs`
(its `#[cfg(test)]` module at line 297 only). **No non-test lines change.**

These tests pass immediately — that is expected and correct here: they pin
existing behaviour the design depends on (spec §3.2), not new behaviour. Per
`mur-tdd` this is the one legitimate case for a test that goes green at once;
name them so a reader knows they are characterization tests.

Build the input as `[system Text, user Text("<project_instructions …>"), …]`
literally — no dependency on `ProjectInstructions`.

Steps:
1. Anthropic `pinned_block_merges_into_current_message_when_no_prior_turns`:
   input `[system, block, current]` → `convo.len() == 1`, role `user`,
   `content[0].text` starts with `<project_instructions`, `content[1].text` is
   the current message. Asserts *which* message.
2. Anthropic `pinned_block_merges_into_first_prior_user_message_not_current`:
   input `[system, block, u1, a1, current]` → `convo.len() == 3`;
   `convo[0].content[0]` is the block and `convo[0].content[1]` is `u1`;
   `convo[2].content` is the current message alone.
3. Anthropic `pinned_block_is_not_in_system_text`: `system_text` from the
   tuple contains no `<project_instructions`.
4. OpenAI `pinned_block_is_standalone_user_at_index_1_before_prior_turns`:
   `[system, block, u1, a1, current]` → `msgs[1].role == "user"`,
   `msgs[1].content` starts with the tag, `msgs[2].content == "u1"`. Add a
   one-line comment: Codex delegates to `OpenAiClient` (`codex.rs:28,39`), so
   this test covers Codex.
5. Ollama `to_ollama_messages_keeps_pinned_block_at_index_1`: same shape via
   `to_ollama_messages`.
6. Commit: `test(llm): characterize where a pinned first user message lands per provider`.

Acceptance:
- [ ] Spec §7.4 all five boxes.
- [ ] `git diff` touches only `#[cfg(test)]` regions.

---

## T4 — Block renderer

**Blocked by:** T1, T2.
**Files:** `mur-agent-runtime/src/project_instructions/{mod,budget,decode,tests}.rs`.

Contract (spec §3.4, §4.1, §5): `render(&self, cwd, cap_bytes) -> Option<Rendered>`
where `Rendered { text, bytes }`; `HEADER` is deleted from this module (the
system paragraph moves to `task_runner.rs` in T5). The module must not import
anything from `task_runner`.

Seams: `render` (public), `discover` (public, unchanged), `budget::fair_share`
(pub(super), pure), `decode::decode_file` (pub(super), pure), `escape_body`
(pub(super), pure). Tests go through these; nothing tests private helpers.

Steps, in order (each RED → GREEN → REFACTOR; keep the 8 existing tests
compiling by updating their assertions to the new format in step 1):

**Format and escaping (`mod.rs`)**
1. RED: `renders_one_file_as_xml_with_relative_path_and_repo_root` — one
   `AGENTS.md` at repo root, cwd a subdir → text matches
   `<project_instructions root="{root}">` … `<file path="AGENTS.md">…</file>` …
   `</project_instructions>`; `bytes == text.len()`. Fails: `render` returns
   `Option<String>`, no `Rendered`, no XML.
2. GREEN: change the signature and the format; rewrite existing assertions in
   `tests.rs` from `### \`…\`` to `<file path="…">`. Existing 8 must be green
   again before moving on.
3. RED: `precedence_preamble_is_emitted_once_before_first_file` — the fixed
   preamble line (spec §3.4) appears exactly once, before the first `<file`.
4. GREEN.
5. RED: `escapes_block_tags_in_file_bodies_case_insensitively` — body contains
   `</PROJECT_INSTRUCTIONS>`, `<File path="x">`, `<not_loaded`, and also
   `<br>` and `Vec<T>` → the first three become `&lt;…`; the last two are
   untouched. Fails: no escaping.
6. GREEN: `escape_body`.
7. RED: `outside_a_repo_root_is_the_cwd` — no `.git` → `root="{cwd}"`,
   `path="AGENTS.md"`.
8. GREEN (may already pass via `discover`; if so, fix the test to assert
   something `discover` does not already guarantee — the `root` attribute).
9. RED: `cwd_change_between_renders_switches_root_and_files` — two repos, two
   `render` calls on the same `ProjectInstructions` → different `root` and
   different file bodies.
10. GREEN.

**Decoding (`decode.rs`)**
11. RED: `nul_byte_anywhere_is_unreadable` → `<not_loaded … reason="unreadable"/>`,
    no `<file>` for it.
12. GREEN: `decode_file(bytes) -> Result<String, Unreadable>` with the NUL check.
13. RED: `mostly_invalid_utf8_is_unreadable` — a 100-byte body with 15 invalid
    bytes → `unreadable`. And `a_few_invalid_bytes_load_lossily` — 100 bytes,
    2 invalid → `<file>` present, contains `\u{FFFD}`.
14. GREEN: lossy decode + 10 % ratio (the threshold is a named `const`).
15. RED: `leading_bom_is_stripped` → body does not start with `\u{FEFF}`.
16. GREEN.
17. RED: `file_deleted_between_discover_and_read_is_skipped` — hard to race;
    instead test through the seam: a `discover` result containing a path that
    no longer exists → no entry, no panic. (If `render` calls `discover`
    internally, expose `render_paths(paths, …)` as `pub(super)` and test that.)
18. GREEN.

**Shadowing (`mod.rs`)**
19. RED: `losers_in_the_same_dir_are_listed_as_shadowed` — `AGENTS.md` +
    `CLAUDE.md` → `<not_loaded path="CLAUDE.md" reason="shadowed"/>`; and
    `a_symlinked_claude_md_is_not_loaded_twice` (existing) additionally asserts
    no `shadowed` entry for the symlink. Fails: `discover` takes the first only.
20. GREEN: `discover` (or a sibling) reports losers; the canonical-dedupe check
    suppresses symlinks to an already-seen file.

**Fair share (`budget.rs`)** — test the pure function first, then through `render`.
21. RED: `fair_share_two_files_root_whole_nested_cut` — sizes `[1024, 102400]`,
    cap 16384 → `[1024, 15360]`. Fails: no `fair_share`.
22. GREEN: `fair_share(sizes: &[usize], cap: usize) -> Vec<usize>` — even split
    then redistribute until stable.
23. RED: `fair_share_three_files_redistributes_remainder` — `[2048, 3072, 30720]`,
    cap 16384 → `[2048, 3072, 11264]` (the design's "deep gets 11 KiB, not
    ~5.3"). Expected values are literals from the spec, not recomputed.
24. GREEN.
25. RED: `fair_share_all_over_share_splits_evenly` — `[10240, 10240, 30720]`,
    cap 16384 → each `5461` (±1 for integer division; assert sum ≤ 16384 and
    each within 1 of 5461).
26. GREEN.
27. RED (through `render`): rewrite
    `budget_cuts_on_a_char_boundary_and_names_what_was_not_loaded` — two files
    per the spec's two-file case, cap passed as `render(cwd, 16384)`; nested
    `<file>` is truncated on a char boundary (CJK content), **and** a
    `<not_loaded path="nested/AGENTS.md" reason="budget"/>` entry is present;
    root `<file>` is whole. Exactly one `budget` entry.
28. GREEN: `render` uses `fair_share` and `clip`.
29. RED: `three_budget_entries_when_all_files_are_cut` (through `render`).
30. GREEN.

**Refusals (`mod.rs`, depends on T2's typed gate)**
31. RED: `no_grant_is_listed_and_deny_and_launch_chain_are_omitted` — three
    files: one outside every grant, one under `deny`, one the (inert →
    use a test chain that protects a path, as `fs_policy` tests do) launch
    chain protects → block contains `reason="no-read-grant"` for the first and
    **no mention of the other two paths at all**. Fails: today all three are
    skipped silently.
32. GREEN: match on `ReadRefusal`.
33. RED: `no_error_display_text_ever_reaches_the_block` — for every variant,
    the block contains none of the substrings `entitled`, `denied by`,
    `launch chain`, `mur agent perm` (the three literals' distinguishing words).
34. GREEN (should already pass; if it does, the test is still kept as a guard).
35. Once-per-path logging: `WARNED: LazyLock<Mutex<HashSet<PathBuf>>>` in
    `mod.rs`. Not unit-tested (tracing output is not a public seam); reviewer
    checks the log-level table (spec §5.6) by reading the code. Add a one-line
    comment on the static naming spec §5.6.

**Wrap**
36. Delete `HEADER`. Update the module doc (no "system prompt"). Clippy clean.
37. Commit(s): `feat(runtime): render project instructions as a <project_instructions> block`
    (optionally one commit per sub-section).

Acceptance:
- [ ] Spec §7.1 all boxes.
- [ ] No file in `project_instructions/` exceeds 800 lines (target ≤ 500).
- [ ] `MAX_PROJECT_INSTRUCTIONS_BYTES` remains the only size literal; the 10 %
      threshold is a named const; no literal `4` for chars-per-token anywhere in
      this module (it has no business knowing tokens).
- [ ] `grep -rn HEADER mur-agent-runtime/src/project_instructions` → nothing.

---

## T5 — Runner: `seed_history` pinned argument, send-time trim, cap, system paragraph

**Blocked by:** none (see step 0).
**Files:** `mur-agent-runtime/src/task_runner.rs` only.

Contract (spec §4.2, §4.3, §5.3, §5.4, §3.3):

- `seed_history(ctx, system, pinned: Option<String>, prior: Vec<RichMessage>, input)`
  → `[system?, pinned?, prior…, current]`.
- `fn pinned_cap_bytes(budget_tokens) -> usize` =
  `min(MAX_PROJECT_INSTRUCTIONS_BYTES, budget_tokens as usize * CHARS_PER_TOKEN_ESTIMATE / 2)`.
- `fn trim_for_send(prior: Vec<RichMessage>, budget_tokens, pinned_len) -> Vec<RichMessage>`
  — `room = budget_tokens.saturating_sub(pinned_len / CHARS_PER_TOKEN_ESTIMATE)`;
  `drop_oldest_turn` while `turn_count > 1 && estimated_tokens > room`.
- `PROJECT_INSTRUCTIONS_RULE: &str` (spec §3.3 text) appended in
  `assemble_system_prompt` right after `WORKING_DIR_RULE`, only when
  `session_cwd` is `Some`.
- Doc comment on `opens_turn` (spec §4.4).

Step 0 — **shim so T5 compiles without T4.** At both call sites (`:1803`,
`:2133`) pass `pinned: None` and `prior: conversations.prior(ctx)` (untrimmed).
Leave the `assemble_system_prompt` call to `p.render(&dir)` as-is with a
`// T6: replace with pinned block` comment. T6 removes all of this. This keeps
`task_runner.rs` the only file T5 edits.

Steps:
1. RED: `seed_history_places_pinned_block_at_index_1_as_user_text` — with a
   stored `ctx` of `[u1, a1]` and `pinned = Some("<project_instructions …>")`
   → `len == 5`, `[0]` system, `[1]` `Text{role:"user"}` starting with the tag,
   `[2]` `u1`, `[4]` current. Fails: no such parameter.
2. GREEN: new signature; update the two call sites (shim) and the two existing
   tests (`:3591`, `:3763`) to pass `None` and the prior vec.
3. RED: `seed_history_without_pinned_matches_baseline` — `None` → same
   4-element output as `seed_history_prepends_prior_conversation` today.
4. GREEN (should pass; keep as guard).
5. RED: `pinned_cap_is_half_the_history_budget_capped_at_max` —
   `pinned_cap_bytes(8_000) == 16_000`; `pinned_cap_bytes(100_000) == 32 * 1024`;
   `pinned_cap_bytes(0) == 0`. Literals from the spec (§5.3).
6. GREEN.
7. RED: `send_time_trim_keeps_pinned_and_newest_turn` — store three turns in
   `ctx` each ~12 chars (3 tokens); `budget_tokens = 10`; `pinned_len = 24`
   (6 tokens) → `room = 4`; result keeps only the newest turn (3 tokens);
   then `seed_history` with that result → block at `[1]`, newest turn at
   `[2..4]`. Fails: no `trim_for_send`.
8. GREEN.
9. RED: `send_time_trim_never_drops_the_last_turn` — one stored turn of 20
   tokens, `room = 4` → the turn survives (same guard as `remember`).
10. GREEN.
11. RED: `room_and_estimated_tokens_use_the_same_divisor` — a synthetic
    `Text` of `N` chars: `estimated_tokens(&[msg])` equals
    `budget_tokens − trim_room(budget_tokens, N)` for `N = 4 * k` (pick
    `N = 400`, `budget = 1_000` → both `100`). Fails only if a literal slips in;
    passes at once — keep as the alignment guard the spec demands.
12. GREEN.
13. RED: `system_prompt_names_the_pinned_block_without_file_contents` — rewrite
    `project_agents_md_follows_the_working_directory_in_the_prompt` (`:7256`):
    with a session cwd, `sys` contains `<project_instructions>` **as a mention
    in the rule text** and does **not** contain `## Project instructions`.
    (Until T6 removes the shim, also assert it does not contain
    `PROJECT-RULE: run cargo fmt` — this assertion will be RED until T6; mark
    the test `#[ignore = "T6"]` if the branch must stay green per-commit, and
    un-ignore it in T6.)
14. GREEN: add `PROJECT_INSTRUCTIONS_RULE` after `WORKING_DIR_RULE`.
15. RED: `no_session_cwd_means_no_project_instructions_rule` — extend the
    existing `no_session_cwd_means_no_working_directory_line` test's sibling.
16. GREEN.
17. Doc comment on `opens_turn`. Clippy clean.
18. Commit: `feat(runtime): seed_history takes a pinned block; send-time trim against the history budget`.

Acceptance:
- [ ] Spec §7.3 boxes 1, 2, 3, 6, 7 (box 4, 5 land in T6), and the `opens_turn`
      doc comment.
- [ ] `grep -n "/ 4\b" task_runner.rs` finds no new literal division.
- [ ] `task_runner.rs` grows by < 150 non-test lines (it is already over the
      800-line rule; do not make the future split harder — no new nested
      modules inside it).

---

## T6 — Integrate, end-to-end tests, docs

**Blocked by:** T4, T5.
**Files:** `mur-agent-runtime/src/task_runner.rs`,
`mur-agent-runtime/src/supervisor_runner.rs` (only if the constructor at `:450`
needs a signature change — expected: none), `README.md`,
`mur-server/dashboard/docs-content/…` (path per the `update-docs` skill),
product page.

Steps:
1. Remove the T5 shim: delete the `p.render(&dir)` call from
   `assemble_system_prompt`; at both call sites compute
   `cap = pinned_cap_bytes(budget_tokens)`, `pinned = project_instructions
   .and_then(|p| p.render(&dir, cap)).map(|r| r.text)`, `prior =
   trim_for_send(conversations.prior(ctx), budget_tokens, pinned_len)`, then
   `seed_history(…)`. Render **once**, before the loop at `:2133`.
   Un-ignore the T5 step-13 test; it must now be green.
2. RED: `pinned_block_is_never_stored_in_conversation_memory` — full turn via
   `new_stub_echo()` with a session cwd containing `AGENTS.md` and a
   `context.task_id`; afterwards `conversations.prior(ctx)` has no message
   containing `<project_instructions`. Fails if step 1 accidentally stores the
   seeded list (it should not — guard).
3. GREEN.
4. RED: `every_tool_loop_step_sends_exactly_one_pinned_block_at_index_1` —
   `RecordingLlm` (`:6299`) with two tool-call responses then an end-turn;
   session cwd with `AGENTS.md`; after the turn, every entry in `seen` contains
   exactly one `<project_instructions` and it is the second message (parse the
   `Debug` string or, better, extend `RecordingLlm` to store `Vec<Vec<RichMessage>>`).
5. GREEN (should pass via step 1 — keep as the multi-step guard).
6. RED: `render_is_called_once_per_turn_not_per_step` — wrap `render` behind a
   counting test double **only if** `ProjectInstructions` is trait-shaped; it is
   a concrete struct today. Alternative seam: write `AGENTS.md` = "A", start a
   turn with `RecordingLlm` whose first response is a tool call that
   **rewrites** `AGENTS.md` to "B" (use the `write_file` tool with a write
   grant); assert step 2's request still carries "A". This tests the observable
   guarantee in spec §3.1 without a mock.
7. GREEN.
8. Run the whole crate: `cargo test -p mur-agent-runtime`; clippy; `cargo fmt`.
9. Docs (CLAUDE.md checklist): `README.md` + docs site + product page get one
   paragraph: MUR agents read `AGENTS.md` / `AGENT.md` / `CLAUDE.md` from the
   repo root down to the working directory; injected as project context, not
   operator rules; cannot widen permissions; capped at 32 KiB or half the
   history budget, whichever is smaller. Use the `update-docs` skill for paths.
10. Commit: `feat(runtime): pin project instructions as the first user message`
    + `docs: project instruction files`.
11. Hand to QA for the manual PINEAPPLE check (spec §7.5) on Anthropic and one
    OpenAI-shaped provider.

Acceptance:
- [ ] Spec §7.3 boxes 4 and 5; §7.6 both boxes.
- [ ] `grep -rn "## Project instructions" mur-agent-runtime/src` → nothing.
- [ ] PR #1475 description updated to say "pinned first user message", with a
      link to the spec.

---

## Verification (before claiming done — `mur-verification`)

```
cargo test -p mur-agent-runtime
cargo clippy -p mur-agent-runtime --all-targets -- -D warnings
cargo fmt --check
wc -l mur-agent-runtime/src/project_instructions/*.rs   # each ≤ 800
grep -rn "## Project instructions\|HEADER" mur-agent-runtime/src/project_instructions  # empty
```

Plus the manual PINEAPPLE check with a real key: turn 1 (no history) and
turn 2 (with history), on Anthropic and one OpenAI-shaped provider. Record the
result in the PR.

## Ownership

- **Coder** — T1–T6 in the order above (T1/T2/T3/T5 may run in parallel via
  `mur-delegate-dev`; T4 after T1+T2; T6 after T4+T5). TDD at every step.
- **QA** — reviews T3 characterization tests against real provider docs;
  runs the manual PINEAPPLE check (spec §7.5) after T6; checks the log-level
  table (spec §5.6) by reading `project_instructions/mod.rs`; confirms the 800-line
  rule with `wc -l`.
- **GitHub Manager** — updates PR #1475 title/body to the new design, links the
  spec, ensures CI green, merges after QA sign-off. No version bump (feature
  ships with the next scheduled release; if it is the release, follow
  CLAUDE.md: bump `Cargo.toml` first, then tag).
- **Human (David)** — decided the eight §9 defaults in the spec on 2026-09-23:
  all accepted as written. T4/T5 proceed with A3 (`render(cwd, cap_bytes)`) and
  A7 (truncated file listed both as `<file>` and `<not_loaded reason="budget"/>`).
