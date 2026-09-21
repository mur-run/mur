# Plan: honour `retry-after` end to end on 429

**Spec:** inline (§ below) — small enough not to warrant a separate design doc.
**Execution skill:** `mur-executing-plans` (one crate plus a one-line comment fix in a second repo; sequential, no delegation).
**Branch:** implement on `fix/llm-retry-after` branched from `origin/main` in a worktree. Do NOT build on `docs/webhook-limiter-comments`, which is the currently checked-out branch and has unrelated dirty files (`mur-core/src/cmd/agent/cli/ui/band.rs`, `band/tests.rs`).

**Goal:** when an upstream — or our own gateway — answers 429 with a `retry-after` header, the runtime waits *that* long instead of its own guess.

**Tech stack:** Rust 2024, `cargo nextest`. Crates: `mur-agent-runtime` only, plus one comment in `mur-model-gateway`. **No new dependency:** `chrono` is already a direct dep (`mur-agent-runtime/Cargo.toml:51`) and `DateTime::parse_from_rfc2822` covers the HTTP-date form. `httpdate` is in `Cargo.lock` but only transitively — do not promote it.

---

## § Spec — the drift this closes

`mur-model-gateway` sends a `retry-after` on its local 429 and its own source comment claims the caller honours it:

```rust
// src/lib.rs:672-675
// The wait is bounded: past `queue_timeout` we answer with a local 429
// rather than leave the caller hanging until *its* timeout. mur-core
// classifies 429 as retry-then-advance and reads `retry-after`, so the
// existing caller chain handles this without changes.
```

**The second half of that sentence is false.** Classification is real; reading `retry-after` is not:

```rust
// mur-agent-runtime/src/llm/mod.rs:356-359
pub fn from_status(status: u16, body: String) -> LlmError {
    match status {
        401 | 403 => LlmError::Auth(status, body),
        429 => LlmError::RateLimit,
```

`from_status` takes `(u16, String)` — no header map ever reaches it, and `LlmError::RateLimit` (mod.rs:301) is a unit variant with nowhere to put a delay. The retry site invents its own schedule instead:

```rust
// mur-agent-runtime/src/task_runner.rs:2793-2795
fn rate_limit_backoff_delay(attempt: u8) -> std::time::Duration {
    RATE_LIMIT_BACKOFF_BASE * (1u32 << u32::from(attempt))
}
```

`RATE_LIMIT_BACKOFF_BASE` is 1s (task_runner.rs:529), so the three attempts are 2s, 4s, 8s.

So against our own gateway, which asks for 5s (`CONCURRENCY_RETRY_AFTER_SECS`), the runtime retries at **2s** — before a permit is plausibly free — burning one of its three attempts on a request that is near-certain to 429 again. Against a real provider asking for 60s it retries at 2s/4s/8s, gives up in 14s, and fails the turn.

The companion outbox has the same hole, and says so in a TODO:

```rust
// mur-agent-runtime/src/companion/outbox/mod.rs:641-642
// TODO(M5.x or later): wire raw HeaderMap from anthropic.rs once that
// surfaces 429 details; for now use deterministic backoff schedule.
```

### Behaviour required

1. `LlmError::RateLimit` carries `Option<Duration>` — the parsed `retry-after`, `None` when absent or unparseable.
2. Parsing accepts **both** RFC 9110 forms: delta-seconds (`retry-after: 5`) and HTTP-date (`retry-after: Wed, 21 Oct 2026 07:28:00 GMT`). A date in the past yields `Duration::ZERO`, not an error.
3. A hostile or fat-fingered upstream cannot park a turn: the honoured value is clamped to `RETRY_AFTER_MAX` (120s). A value above the clamp is used *at the clamp*, not discarded.
4. When `retry_after` is `None`, behaviour is **byte-for-byte what it is today** — the existing exponential schedule. This change adds a signal; it does not retune the fallback.
5. `classify()` still returns `RetryThenAdvance` for every `RateLimit`, whatever the delay. Disposition is not a function of the header.
6. The outbox pause uses the header when present (clamped as above), its `RETRY_BACKOFF_SECS` table when not, and the TODO at mod.rs:641 is deleted rather than reworded.

### Non-goals

- No `retry-after` on 503/529. Only 429 in this plan.
- No change to `MAX_RATE_LIMIT_RETRIES` (3) or to `RETRY_BACKOFF_SECS`.
- No new env vars.

## Global constraints (every task)

- `cargo nextest run -p mur-agent-runtime <filter>` for tests; `cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings` and `cargo fmt -p mur-agent-runtime` before every commit. **Read exit codes, not grep output.**
- Never sleep a real 120s in a test. Assert on the *returned `Duration`*, or drive `tokio::time` with `start_paused = true`.
- One commit per task, each green on its own.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

---

## Task 0 — branch and baseline

- [x] `git worktree add .worktrees/retry-after -b fix/llm-retry-after origin/main`
- [x] Baseline: `cargo nextest run -p mur-agent-runtime` green before touching anything. Record the pass count in the commit body.
- [x] Commit this plan file (it is currently untracked in the main worktree).

## Task 1 — the variant carries the delay (RED first)

The variant change is mechanical but wide: **17 occurrences of `LlmError::RateLimit` across 9 files** match or construct it. Do the signature first, let the compiler drive the rest.

- [x] **RED.** New unit tests in `mur-agent-runtime/src/llm/mod.rs` tests module:
  - `retry_after_parses_delta_seconds` — `"5"` → `Some(5s)`.
  - `retry_after_parses_http_date` — a date 30s in the future → `Some(~30s)` (allow ±2s slack; the clock moves).
  - `retry_after_past_date_is_zero` — a date in 2020 → `Some(ZERO)`.
  - `retry_after_garbage_is_none` — `"soon"`, `""`, `"-1"` → `None`.
  - `retry_after_is_clamped` — `"99999"` → `Some(RETRY_AFTER_MAX)`.
  - `classify_ignores_retry_after` — both `RateLimit(None)` and `RateLimit(Some(60s))` → `RetryThenAdvance`.
- [x] Change the variant at mod.rs:301 to `RateLimit(Option<Duration>)`. Keep the `#[error("rate limit")]` string when `None`; render `rate limit (retry after Ns)` when `Some` — the error text reaches task JSON and a human reading it deserves the number.
- [x] Add `pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(120);` with a comment saying *why* 120 (a fleet step's own deadline is the next thing to fire; anything longer should fail fast and let the fallback chain route).
- [x] Add `pub fn parse_retry_after(value: &str) -> Option<Duration>` implementing §2–3.
  - **Correction (Task 1).** The plan missed a second `retry-after` reader that
    already exists: `mur-agent-runtime/src/durable/rate_limit.rs:35`,
    `parse_anthropic_429`, tested by `tests/durable_rate_limit.rs`. It is NOT
    duplicated work and the two must stay separate — it answers "when does this
    *suspended run* resume" (also reads `anthropic-ratelimit-*-reset`, ×6 on a
    529, returns an absolute timestamp, deliberately unclamped), while
    `parse_retry_after` answers "do we sleep inside this live turn" (clamped to
    `RETRY_AFTER_MAX`). Each function now carries a doc comment pointing at the
    other so the next reader does not try to merge them.
- [x] Add `pub fn from_status_with_headers(status: u16, body: String, headers: &reqwest::header::HeaderMap) -> LlmError`. Keep `from_status(status, body)` as a thin wrapper passing an empty map — **`mur-agent-runtime/src/llm/ollama.rs:128,184` and both provider mappers call it and the existing tests at mod.rs:560-580 must keep compiling unchanged.**
  - **Correction (Task 1).** "must keep compiling unchanged" was wrong. The
    *callers* of `from_status` are untouched, as planned, but any test that
    **pattern-matches** `LlmError::RateLimit` cannot survive a unit→tuple
    variant change: `matches!(x, LlmError::RateLimit)` is a unit pattern and
    stops compiling. Six such sites needed `(_)` or `(None)`.
- [x] Fix the fallout. Known sites: `mod.rs:359,412,563,588`; `task_runner.rs:2240`; `client_builder.rs:468-469`; `stub.rs:67`; `companion/outbox/generate.rs:112`; `companion/outbox/tests/i18n.rs:135,270`; `tests/companion_rate_limit_i18n.rs:57,307`; `tests/llm_stub.rs:36`; `llm/fallback/tests.rs:215`. Every one of these is `RateLimit` → `RateLimit(None)`; none of them should gain a value in this task.
- [x] **GREEN.** `cargo nextest run -p mur-agent-runtime llm::` and the full suite.

## Task 2 — providers surface the header

All six 429 sites already hold `resp` before consuming the body, so the header map is reachable — but **`anthropic/mod.rs:699` calls `resp.text().await` before the status check**, so capture `let headers = resp.headers().clone();` *above* that line or it is gone.

- [ ] **RED.** Table test per adapter: a 429 response carrying `retry-after: 7` maps to `RateLimit(Some(7s))`; the same 429 without the header maps to `RateLimit(None)`. Put them beside the existing mapper tests (`openai/tests.rs`, `anthropic/tests.rs`) and in `ollama.rs`'s test module.
- [ ] `map_openai_error` and `map_anthropic_error` take a `&HeaderMap` and pass it through every `from_status` tail — including the parse-failure early return (`openai/mod.rs:22`, `anthropic/mod.rs:48`), which is the path a bare-body 429 actually takes.
- [ ] Update call sites: `openai/mod.rs:398,469`; `anthropic/mod.rs:702,739`; `ollama.rs:128,184`.
- [ ] Update the mapper tests at `openai/tests.rs:237,252` and `anthropic/tests.rs:624,636,663,672` for the new argument.
- [ ] **GREEN** + clippy + fmt.

## Task 3 — the agentic loop waits the asked-for time

- [ ] **RED.** Test `rate_limit_retry_honours_retry_after`: a stub returning `RateLimit(Some(45s))` makes the loop sleep 45s, not 2s. Use `#[tokio::test(start_paused = true)]` and assert on advanced virtual time; do not wall-clock it.
- [ ] Test `rate_limit_retry_falls_back_to_backoff`: `RateLimit(None)` still gives 2s/4s/8s — this is the regression guard for §4.
- [ ] At `task_runner.rs:2240`, bind the delay: `Some(d) => d.min(RETRY_AFTER_MAX)`, `None => rate_limit_backoff_delay(rate_limit_attempt)`. Leave `MAX_RATE_LIMIT_RETRIES` alone.
- [ ] Extend the existing `tracing::warn!` with `source = "retry-after" | "backoff"` so a support log says which one fired.
- [ ] **GREEN.**

## Task 4 — the companion outbox stops guessing

- [ ] **RED.** Extend the i18n outbox tests: a translate 429 carrying `retry-after: 240` pauses until `now + 240s`, not `now + 30s` (`RETRY_BACKOFF_SECS[0]`).
- [ ] `GenerateResult::RateLimit` carries `Option<Duration>`; thread it from `generate.rs:112` to the two pause sites (`outbox/mod.rs:433` and `:648`).
- [ ] `resume_at = now_utc + header.min(RETRY_AFTER_MAX)` when present, else `backoff_for_attempt(attempt)` exactly as today. Attempt counting and the terminal drop after four attempts are unchanged.
- [ ] **Delete** the TODO at `outbox/mod.rs:641-642` — it is done, and a stale TODO is worse than none.
- [ ] **GREEN.**

## Task 5 — make the gateway comment true

- [ ] In `~/Projects/mur-model-gateway/src/lib.rs:672-675`, the claim is now accurate but vague. Replace "reads `retry-after`" with the specific: the runtime parses it into `LlmError::RateLimit`, clamps at 120s, and retries up to 3 times. Note the required runtime version so a future reader can tell when the claim started being true.
- [ ] Separate PR in that repo. **Do not** bundle it with the runtime change.
- [ ] Sanity-check `CONCURRENCY_RETRY_AFTER_SECS = 5` against the new behaviour: with the header honoured, three attempts now span ~15s rather than 14s of exponential — still inside `DEFAULT_QUEUE_TIMEOUT` (30s). No change needed; record the arithmetic in the PR body.

## Task 6 — verification and PR

- [ ] `cargo fmt -p mur-agent-runtime` (and `cargo fmt --check` for the whole workspace — **if it reports a file this branch never touched, fix it anyway**; CI is fail-fast, and a pre-existing `fmt` drift cancels the macOS and Windows jobs so nothing gets verified).
- [ ] `cargo clippy --all-targets --no-deps -- -D warnings` workspace-wide (the variant change reaches other crates' matches).
- [ ] `cargo nextest run` full workspace; pass count must be ≥ the Task 0 baseline.
- [ ] `gh pr create`, body covering: the false comment that started it, the six honoured sites, and the §4 no-header-no-change guarantee.
- [ ] Watch CI to green on all three platforms before calling it done.
