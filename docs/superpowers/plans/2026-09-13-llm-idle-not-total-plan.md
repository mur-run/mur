# Plan — LLM clients bound idleness, not generation (#1287)

> **Execute with `mur-executing-plans`.** Tick every `- [ ]` in this file as
> you go: it is the durable ledger and survives context compaction.

**Goal.** No clock may end a live LLM request; a stream that stops sending is
bounded by an idle timer measured where activity is observable, and a
non-stream request is bounded only by its turn.

**Spec.** `docs/superpowers/specs/2026-09-13-llm-idle-not-total-design.md`
(revised once after review; read §7 before starting — the first draft's
decorator design was wrong and the reasons are the constraints here).

**Architecture.** One shared `reqwest::ClientBuilder` in `llm/mod.rs` carries
`.no_proxy()` and `connect_timeout` and nothing else. One shared
`StreamActivity` in `llm/mod.rs` owns the two idle bounds; the three provider
stream loops await their next body chunk through it, so any bytes at all count
as life. On elapse each provider returns what it had already assembled with
`StopReason::Interrupted`, and `task_runner` marks it exactly where it already
marks `MaxTokens`.

**Tech stack.** Rust 2024, tokio, reqwest 0.12, `cargo nextest`.

## Global Constraints

Copied verbatim from CLAUDE.md and the spec; every task includes them.

- **No hardcoded values.** Use constants, config, or env vars.
- **Single source file ≤ 800 lines.** When approaching the limit, split into
  submodules following the same structural pattern as siblings. **Pure code
  movement first; behavior changes in a separate PR.**
- **Read narrowly.** Prefer LSP queries and `grep` over reading whole files.
- Build/test env for this crate: `ORT_STRATEGY=download`,
  `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`, `RUST_MIN_STACK=33554432`.
- `cargo nextest`, never bare `cargo test`. Read the **exit code**; never
  grep the output, because a pipe swallows the exit code.
- `grep` is shimmed to ugrep here — use `command grep`.
- Clippy must be run as `--all-targets -- -D warnings`; a `--lib` run hides
  rotting test targets.
- The Hub (`mur-hub-gui`) is workspace-excluded. Its check runs **last**.
- Commit messages in English. End each with the Co-Authored-By line.

### File sizes, stated up front rather than discovered

| File | Now | After | Note |
|---|---|---|---|
| `llm/openai.rs` | **791** | split in Task 0 | 9 lines under the limit; anything added breaks §4, so Task 0 splits it as pure movement in its **own PR** |
| `llm/mod.rs` | 618 | ~700 | stays under |
| `llm/ollama.rs` | 314 | ~330 | stays under |
| `llm/client_builder.rs` | 471 | ~490 | stays under |
| `llm/anthropic.rs` | **1400** | ~1430 | **already 1.75× over, pre-existing.** This plan does not split it: that is a 1400-line refactor against a file this feature touches in three places, and §4 says behaviour changes go in a separate PR from movement. Recorded as a known deviation, not a check to fail. |
| `task_runner.rs` | **6174** | ~6200 | already 7.7× over, pre-existing; same reasoning |

Do **not** write a completion criterion that says these files shrink. Last
project's plan did and it was the criterion that was wrong, not the code.

## File structure

| File | Responsibility after this plan |
|---|---|
| `mur-agent-runtime/src/llm/openai/mod.rs` | *(new, Task 0)* everything `openai.rs` held except its tests |
| `mur-agent-runtime/src/llm/openai/tests.rs` | *(new, Task 0)* the `#[cfg(test)] mod tests` body, moved verbatim. Mirrors the existing `llm/fallback/{mod.rs,tests.rs}` |
| `mur-agent-runtime/src/llm/mod.rs` | adds `LLM_CONNECT_TIMEOUT_SECS`, the timeouts on `llm_client_builder()`, `StopReason::Interrupted`, `STREAM_IDLE_TRUNCATION_MARKER`, and `StreamActivity` |
| `mur-agent-runtime/src/llm/client_builder.rs` | builds its guarded client through `llm_client_builder()` |
| `mur-agent-runtime/src/llm/{ollama,openai/mod,anthropic}.rs` | delete the per-file timeout constants; await each body chunk through `StreamActivity`; return a partial on elapse |
| `mur-agent-runtime/src/task_runner.rs` | mark an `Interrupted` reply beside the existing `MaxTokens` marking |
| `mur-agent-runtime/Cargo.toml` | tokio `test-util`, dev-only |
| `mur-core/src/cmd/agent_companion/preview.rs` | unchanged code; covered by a test that its three branches carry the shared builder's properties |

## Task 0 — split `openai.rs` (pure movement, its OWN PR)

**Why its own PR:** the file is 791 lines and §4 forbids growing it. Nothing
in this task changes behaviour, so its diff must be reviewable as a move.

**Interfaces — Produces:** module path `crate::llm::openai` continues to
resolve to the same public items (`OpenAiClient` and its constructors). No
consumer changes.

- [x] Confirm the branch and the starting point:
      `git branch --show-current` → `feat/llm-idle-not-total`;
      `wc -l mur-agent-runtime/src/llm/openai.rs` → `791`.
- [x] `mkdir mur-agent-runtime/src/llm/openai`
- [x] `git mv mur-agent-runtime/src/llm/openai.rs mur-agent-runtime/src/llm/openai/mod.rs`
- [x] Move lines from `#[cfg(test)]` (was line 589) to end of file out of
      `mod.rs` and into `mur-agent-runtime/src/llm/openai/tests.rs`, dropping
      the outer `#[cfg(test)] mod tests {` wrapper and its closing brace, and
      keeping every `use` inside it. In `mod.rs`, replace what you removed
      with exactly:
      ```rust
      #[cfg(test)]
      mod tests;
      ```
      The sibling pattern is `llm/fallback/{mod.rs,tests.rs}`; open it if the
      shape of the split is unclear.
- [x] In `tests.rs`, the moved code referred to items by `super::*` **from
      inside** `mod tests`, which was one level shallower. Fix imports by
      compiling, not by guessing: the first `use super::*;` still means the
      parent module, so it usually needs no change. Any `super::super::`
      becomes `super::`.
- [x] `wc -l mur-agent-runtime/src/llm/openai/mod.rs mur-agent-runtime/src/llm/openai/tests.rs`
      → `mod.rs` **under 800**, and the two together within one or two lines **Recorded: mod.rs 590, tests.rs 200, total 790 vs 791 — `mod tests {` became `mod tests;`.**
      of 791. **Recorded: mod.rs 590, tests.rs 200, total 790 vs 791 — `mod tests {` became `mod tests;`.**
- [x] Prove it is a pure move — the production half must be untouched:
      ```sh
      git show HEAD:mur-agent-runtime/src/llm/openai.rs | sed -n '1,588p' > /tmp/before.rs
      sed -n '1,588p' mur-agent-runtime/src/llm/openai/mod.rs > /tmp/after.rs
      diff /tmp/before.rs /tmp/after.rs; echo "diff exit=$?"
      ```
      Expect only the trailing `#[cfg(test)] mod tests;` to differ, and
      **nothing** inside any function body. If a function body differs, you
      edited instead of moved — revert and redo.
- [x] Build and test:
      ```sh
      export ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432
      cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo "clippy exit=$?"
      cargo nextest run -p mur-agent-runtime; echo "nextest exit=$?"
      ```
      Both `0`. The nextest count must equal the count on `origin/main` —
      **Recorded: 1086 passed / 6 skipped before the move, 1086 / 6 after, both exit 0.** Equal, so nothing stopped being compiled.
      record both numbers here, because a test that silently stopped being
      compiled is the failure mode a moved `mod` invites.
- [x] Commit: `refactor(llm): split openai.rs into openai/{mod,tests}.rs (pure move)`
- [x] Open PR 1 with the `diff exit=0` output and both nextest counts in the
      body. **Do not tick any later task's checkbox on this branch** — that
      would put a docs change into a PR whose claim is "movement only".
- [x] Merge PR 1, then `git fetch origin && git rebase origin/main`.

## Task 1 — the shared builder: one clock, and the proxy gap closed

Spec D1, D2. Also settles §1.3's open question.

**Interfaces — Produces:**
```rust
// llm/mod.rs
pub(crate) const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;
pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder;  // + .connect_timeout
```
**Consumes:** nothing from Task 0 beyond the moved module path.

- [x] Write the falsifiable test FIRST, and run it against the **unfixed**
      code so you see what today does. In `llm/client_builder.rs` tests:
      ```rust
      /// §1.3 of the spec, as a question rather than a claim: the guarded
      /// client is built with `reqwest::ClientBuilder::new()`, which has no
      /// `.no_proxy()`, while `llm_client_builder()` has one and a test
      /// guarding it. If an ambient proxy captures this client's traffic,
      /// `HostGuard` — installed as a DNS resolver, and the ONLY host
      /// enforcement for this process's egress — never resolves the real
      /// destination, so `allow_hosts` is not applied to it.
      #[tokio::test]
      async fn a_restricted_agent_ignores_an_ambient_proxy() { /* see below */ }
      ```
      Shape it on `proxy_isolation_tests::llm_client_builder_ignores_ambient_http_proxy`
      in `llm/mod.rs` (a real `TcpListener` standing in for the proxy). Build
      the client through `build_bare_client` with a `Restricted` profile whose
      `allow_hosts` does **not** contain the target host, with `HTTP_PROXY`
      pointed at the listener. Assert the listener accepts **no** connection.
- [x] **Already settled, 2026-09-13, by a throwaway probe run before Task 0 —
      the answer is in the spec's §1.3.** The bypass is **real**:

      ```
      PROBE RESULT: the proxy CAPTURED it — HostGuard bypassed.
      status=Ok(200) first line=Some("GET http://blocked.example.com/v1/messages HTTP/1.1")
      ```

      A bare `ClientBuilder` with `HostGuard::restricted(vec![])` and
      `HTTP_PROXY` set reached a host the allowlist forbade. The absolute-form
      request line proves it was proxied, not resolved. So the test above is a
      genuine guard: it must fail without `.no_proxy()` and pass with it.
      Confirm that ordering once while implementing — write the test, watch it
      fail, then apply the builder change.
- [x] In `llm/mod.rs`, add above `llm_client_builder`:
      ```rust
      /// Time allowed to establish a TCP connection to an LLM endpoint. The
      /// only clock on the client: `read_timeout` is deliberately absent
      /// because reqwest polls it while the response head is still
      /// outstanding (`async_impl/client.rs:3053`), which makes any value a
      /// hard ceiling on server think time — see the spec's D2 and D5.
      pub(crate) const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;
      ```
      and change the builder to:
      ```rust
      pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
          reqwest::Client::builder()
              .no_proxy()
              .connect_timeout(std::time::Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
      }
      ```
- [x] In `llm/client_builder.rs`, replace the `guarded_http` build (was line
      189) with:
      ```rust
      let guarded_http = crate::llm::llm_client_builder()
          .dns_resolver(std::sync::Arc::new(host_guard))
          .build()
          .context("failed to build guarded HTTP client")
          .map_err(GuardedHttpBuildError)?;
      ```
- [x] Delete `LLM_REQUEST_TIMEOUT_SECS` and `LLM_CONNECT_TIMEOUT_SECS` from
      all three of `llm/ollama.rs`, `llm/openai/mod.rs`, `llm/anthropic.rs`,
      and in each self-building constructor drop the `.timeout(...)` and
      `.connect_timeout(...)` calls so it reads `crate::llm::llm_client_builder().build()`.
- [x] Re-run the proxy test. It must now pass whichever way it went before.
- [x] `command grep -rn "LLM_REQUEST_TIMEOUT_SECS" mur-agent-runtime/src mur-core/src` → **no hits**.
- [x] Add the two guards (**the builder guard was mutation-verified**: adding
      `.read_timeout(600s)` to the builder makes it FAIL, removing it makes it
      pass, so it is not a tautology. **Honest limitation recorded in the test
      itself**: `reqwest::Client`'s `Debug` does not print `connect_timeout`,
      so the guard proves only that the two forbidden clocks are absent, not
      that the connect clock is applied — proving that behaviourally needs a
      TCP connect that stalls rather than refuses, i.e. an unroutable address
      and a flaky test):
      ```rust
      // llm/mod.rs tests
      #[test]
      fn the_builder_has_a_connect_clock_and_no_other() { /* build; assert Debug output names connect_timeout and not read_timeout */ }
      ```
      `reqwest::Client`'s `Debug` prints its configured timeouts (see
      `async_impl/client.rs:2959`), so assert on that string rather than on a
      private field. If the `Debug` shape turns out not to include it, fall
      back to a behavioural assertion: a server that accepts and stalls past
      any former backstop keeps the request pending (drive it with
      `tokio::time::pause` from Task 4's feature).
- [x] Verify and commit:
      ```sh
      cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo $?
      cargo nextest run -p mur-agent-runtime; echo $?
      ```
      Commit: `fix(llm): one client builder, connect clock only, no ambient proxy (#1287)`

## Task 2 — `StreamActivity`: bound silence, not work

Spec D3, D3a, D4, D7.

**Interfaces — Produces:**
```rust
// llm/mod.rs
pub(crate) struct StreamActivity { /* private */ }

impl StreamActivity {
    /// Bounds from `MUR_LLM_FIRST_DELTA_TIMEOUT_SECS` /
    /// `MUR_LLM_IDLE_TIMEOUT_SECS`, else the constants.
    pub(crate) fn from_env() -> Self;

    /// Await one chunk-producing future, bounded. `Ok(None)` = the stream
    /// ended; `Err(LlmError::Timeout)` = the bound elapsed.
    ///
    /// Generic on purpose: `resp.chunk()` yields `bytes::Bytes`, and `bytes`
    /// is NOT a direct dependency of this crate (checked) while reqwest does
    /// not re-export it (checked). Keeping the element type inferred at the
    /// three call sites avoids adding a dependency purely to name a type,
    /// and it is also what makes the timing rule testable without a socket.
    pub(crate) async fn bounded<T>(
        &mut self,
        fut: impl std::future::Future<Output = Result<Option<T>, LlmError>>,
    ) -> Result<Option<T>, LlmError>;
}
pub(crate) const LLM_FIRST_CHUNK_TIMEOUT_SECS: u64 = 300;
pub(crate) const LLM_STREAM_IDLE_TIMEOUT_SECS: u64 = 120;
```
**Consumes:** `LlmError::Timeout` (exists), `llm_client_builder` (Task 1).

- [x] Write the timing tests first, against a generic private core so no HTTP
      is needed:
      ```rust
      impl StreamActivity {
          /// The timing rule itself, generic so the tests can drive it with a
          /// plain sleep and the providers can pass `resp.chunk()` without
          /// this crate having to name `bytes::Bytes`.
          pub(crate) async fn bounded<T>(
              &mut self,
              fut: impl std::future::Future<Output = Result<Option<T>, LlmError>>,
          ) -> Result<Option<T>, LlmError> {
              let bound = if self.seen_any {
                  self.idle
              } else {
                  self.first
              };
              match tokio::time::timeout(bound, fut).await {
                  Err(_) => Err(LlmError::Timeout),
                  Ok(Err(e)) => Err(e),
                  Ok(Ok(None)) => Ok(None),
                  Ok(Ok(Some(v))) => {
                      // D3a: arrival is the activity. Recorded HERE, before
                      // any caller interprets the bytes, so a chunk carrying
                      // only tool-argument fragments or a usage frame counts.
                      self.seen_any = true;
                      Ok(Some(v))
                  }
              }
          }
      }
      ```
      Tests, all on a `tokio::time::pause`d clock:
      - first chunk inside the first bound → `Ok(Some(_))`, and a later gap is
        then judged against the **idle** bound, not the first one;
      - no first chunk → `Err(LlmError::Timeout)`;
      - `Ok(None)` passes through as end-of-stream, never as a timeout;
      - an inner `Err` passes through unchanged (a transport error must not be
        relabelled `Timeout`, because `classify` treats them differently).
- [x] Implement `from_env`: parse each variable, and on an unparseable value
      keep the constant and `tracing::warn!` once naming the variable — a typo
      must not disable the bound. Test both.
- [x] Verify and commit:
      ```sh
      cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo $?
      cargo nextest run -p mur-agent-runtime -E 'test(/stream_activity/) or test(/bounded/)'; echo $?
      ```
      Commit: `feat(llm): StreamActivity bounds stream silence, not generation (#1287)`

**Plan correction made during execution.** Task 4 was where the plan put the
tokio `test-util` dev-dependency, but Task 2's own tests need
`#[tokio::test(start_paused = true)]`, so the manifest change moved here. Task
4's first step is therefore already done.

**Mutation-verified**, because seven green tests prove nothing on their own:
collapsing `bounded` to a single bound (`let bound = self.first`) makes
`the_first_bound_covers_cold_start_and_the_idle_bound_takes_over_after` FAIL
and the other six still pass. D4 is the decision that test defends, and it is
the one a reviewer would most reasonably suspect of being decoration.

## Task 3 — wire the three loops and return partials

Spec D3, D6, §3.3. This is the task the first draft got wrong, so read §7 F1.

**Interfaces — Produces:**
```rust
// llm/mod.rs
pub enum StopReason { EndTurn, ToolUse, MaxTokens, Interrupted }
pub const STREAM_IDLE_TRUNCATION_MARKER: &str =
    "\n\n[output truncated: the model stopped sending]";
```
**Consumes:** `StreamActivity::{from_env, bounded}` (Task 2).

All three loops today are the **identical line**:
```rust
while let Some(chunk) = resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e))? {
```
at `ollama.rs:198`, `openai/mod.rs:477` (was `openai.rs:477`), `anthropic.rs:806`.

- [x] Add the `Interrupted` variant and the marker to `llm/mod.rs`. Compile
      and let the non-exhaustive `match` errors list every site:
      `cargo check -p mur-agent-runtime --all-targets 2>&1 | command grep -c "non-exhaustive"`.
      Decide each one as "behaves like `MaxTokens`" (truncated) unless the site
      says otherwise, and record which sites you touched in this file.
- [x] In each of the three loops, replace the `while let` head with:
      ```rust
      let mut activity = crate::llm::StreamActivity::from_env();
      let mut interrupted = false;
      loop {
          let next = activity
              .bounded(async { resp.chunk().await.map_err(|e| LlmError::from_reqwest(&e)) })
              .await;
          match next {
              Ok(Some(chunk)) => { /* the existing body, unchanged */ }
              Ok(None) => break,
              Err(LlmError::Timeout) => { interrupted = true; break; }
              Err(e) => return Err(e),
          }
      }
      ```
      The `async` block borrows `resp` mutably for exactly the duration of the
      await, which is why `bounded` takes a future rather than the response.
      Do not otherwise touch the body. The point of D3a is that the body is
      irrelevant to liveness.
- [x] **Ollama** (`ollama.rs`): after the loop, before the `text.is_empty()`
      guard:
      ```rust
      if interrupted {
          if text.is_empty() {
              return Err(LlmError::Timeout);   // nothing usable; safe to retry
          }
          stop_reason = StopReason::Interrupted;
      }
      ```
      Ollama has no tool calls (`tool_calls: vec![]`), so text is the only
      usable content.
- [x] **OpenAI** (`openai/mod.rs`): the existing assembly parses each
      fragment's arguments with `unwrap_or(empty object)`. On a normal end that
      is fine; on an interruption it would hand the model a call with
      **invented empty arguments**. Split the rule:
      ```rust
      let tool_calls: Vec<crate::llm::ToolCallResult> = partial
          .into_values()
          .filter(|p| !p.name.is_empty())
          .filter_map(|p| {
              let input = if p.arguments.trim().is_empty() {
                  // A no-argument call legitimately sends "".
                  serde_json::Value::Object(Default::default())
              } else {
                  match serde_json::from_str(&p.arguments) {
                      Ok(v) => v,
                      // Unparseable args mean the fragment stream stopped
                      // mid-JSON. Dropping the call tells the model plainly
                      // that it did not happen; substituting `{}` would run
                      // the tool with arguments the model never chose.
                      Err(_) if interrupted => return None,
                      Err(_) => serde_json::Value::Object(Default::default()),
                  }
              };
              Some(crate::llm::ToolCallResult { call_id: p.id, tool_name: p.name, input })
          })
          .collect();
      if interrupted {
          if text.is_empty() && tool_calls.is_empty() {
              return Err(LlmError::Timeout);
          }
          stop_reason = StopReason::Interrupted;
      }
      ```
      Keeping the non-interrupted `Err(_)` arm as `{}` preserves today's
      behaviour exactly; only the interrupted path is new.
- [x] **Anthropic** (`anthropic.rs`): `cur_tool` is committed only at
      `content_block_stop`, so a call interrupted mid-arguments is already
      dropped — no filter needed. Give `finish_stream` the flag:
      ```rust
      fn finish_stream(
          acc: StreamAccum,
          model: String,
          interrupted: bool,
      ) -> Result<LlmResponse, LlmError> {
          if acc.text.is_empty() && acc.tool_calls.is_empty()
              && acc.stop_reason != StopReason::MaxTokens
          {
              // An interruption with nothing assembled is a timeout, not a
              // malformed response: it is retryable, and `InvalidResponse`
              // would `Stop` the fallback chain.
              return Err(if interrupted {
                  LlmError::Timeout
              } else {
                  LlmError::InvalidResponse("empty streamed response".into())
              });
          }
          let stop_reason = if interrupted { StopReason::Interrupted } else { acc.stop_reason };
          Ok(LlmResponse { text: acc.text, input_tokens: acc.input_tokens,
              output_tokens: acc.output_tokens, model,
              tool_calls: acc.tool_calls, stop_reason })
      }
      ```
      Update the call site and the existing
      `finish_stream_errors_on_truly_empty_response` test
      (`anthropic.rs:1251`) to pass `false`.
- [x] Tests — one per provider, named individually, because the rule now lives
      in three loops (spec §5 tests 4, 5, 6, 8, 9, 10). Drive them with a real
      `tokio::net::TcpListener` that writes a hand-built partial response and
      then holds the socket open without closing it; `httpmock` cannot express
      "send some bytes then stall". The shape to copy is
      `llm/mod.rs::proxy_isolation_tests`.
      - [x] `ollama`: gaps inside the bound → every delta forwarded, `EndTurn`,
            no marker.
      - [x] `openai`: same.
      - [x] `anthropic`: same.
      - [x] **F1's guard, `openai`:** one text delta, then only `tool_calls`
            fragments for longer than the idle bound, then a normal end → the
            call completes with its tool call intact and is **not**
            `Interrupted`. This is the test the first design would have failed.
      - [x] **F1's guard, `anthropic`:** the same with `input_json_delta`.
      - [x] **F2's guard:** `thinking: true` events keep a stream alive, and
            the thinking text does not appear in `LlmResponse.text`.
      - [x] Stops after three text deltas: `Ok`, `Interrupted`, text is those
            three, and the sink received exactly **three** deltas. The fourth
            (the marker) belongs to Task 4.
      - [x] Interrupted mid-arguments: the incomplete call is absent from
            `tool_calls`; `Interrupted` with the preceding text, or `Timeout`
            if there was none.
      - [x] First chunk never arrives: `Err(LlmError::Timeout)` and the sink
            received nothing.
- [x] Verify and commit:
      ```sh
      cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo $?
      cargo nextest run -p mur-agent-runtime; echo $?
      ```
      Commit: `feat(llm): a stream that stops sending yields its partial, not an error (#1287)`

**Three deviations from this task as written, all recorded rather than quietly absorbed.**

1. **The compiler did not find the decision sites.** The plan's first step said
   to add the variant and let non-exhaustive `match` errors list every site.
   `cargo check --all-targets` reported **zero**: every `match` maps a provider
   string INTO `StopReason`, and consumers use equality. The sites were found by
   hand and are tabulated in the spec's §3.4. One of them needed a real
   decision: `task_runner.rs:2373` ends the turn on
   `tool_calls.is_empty() || EndTurn`, so an `Interrupted` response carrying
   complete calls runs them — kept deliberately, because skipping after
   `history.push(ToolUse)` would leave an orphan `tool_use` with no
   `tool_result`, which is invalid for the next Anthropic request.
2. **`StopKind::StreamInterrupted` added to the ledger**, which this task did
   not plan. Mapping an interrupted turn to `end_turn` would write a falsehood
   into a durable audit record. `StopKind` is matched exhaustively twice, so
   those two sites were compiler-enforced.
3. **The paused clock does not work for provider tests, and a discriminator
   caught it.** The fragments-count-as-activity test passed, then passed AGAIN
   with the idle bound set below the gap — impossible unless the gap did not
   exist. Cause: a task blocked on socket I/O counts as idle for auto-advance,
   so tokio jumps to the fake server's next sleep and every gap collapses. The
   16 provider tests moved to a real clock with one-second bounds and gaps of a
   few hundred milliseconds; their wall-clock times are the evidence the gaps
   are real (`ollama_slow_but_steady_generation_is_never_cut_off` takes 6.06 s,
   `openai_tool_argument_fragments_count_as_activity` 2.44 s). The timing rule
   itself is still unit-tested on a paused clock, where nothing blocks on I/O.
   The tests live in one new file, `llm/stream_idle_tests.rs`, rather than three
   modules: the harness is identical for all three providers and `anthropic.rs`
   is already 1400 lines against §4's 800.

## Task 4 — mark it where `MaxTokens` is already marked

Spec D6a, §3.5.

**Consumes:** `StopReason::Interrupted`, `STREAM_IDLE_TRUNCATION_MARKER` (Task 3).

- [x] `mur-agent-runtime/Cargo.toml`, under the existing `[dev-dependencies]`
      (line 142):
      ```toml
      # `tokio::time::pause` for the stream-idle tests; dev-only so no
      # production build gains test-util.
      tokio = { workspace = true, features = ["test-util"] }
      ```
- [x] In `task_runner.rs`, beside `mark_max_tokens_truncation` (line 1712),
      add the sibling:
      ```rust
      /// An interrupted stream is truncated for every purpose the max_tokens
      /// marker exists for, so it reuses the same `truncated` usage flag
      /// (task_runner.rs:1326) rather than adding a wire field.
      fn mark_stream_interruption(&self, task_id: &str, resp: &mut crate::llm::LlmResponse) {
          self.last_turn_truncated.store(true, Ordering::Relaxed);
          tracing::warn!(
              agent = self.hook_ctx.as_ref().map(|c| c.agent_name.as_str()).unwrap_or("<unknown>"),
              task_id,
              model = %resp.model,
              "llm stream stopped sending; partial reply kept (visible marker appended)"
          );
          resp.text.push_str(crate::llm::STREAM_IDLE_TRUNCATION_MARKER);
      }
      ```
- [x] At the `MaxTokens` site (lines 2329-2339), add the parallel branch so the
      marker reaches the returned reply, the streamed output **and** the
      persisted history — the three destinations that comment already names:
      ```rust
      if resp.stop_reason == crate::llm::StopReason::Interrupted {
          self.mark_stream_interruption(task_id, &mut resp);
          if let Some(s) = &sink {
              let _ = s.send(crate::llm::StreamDelta {
                  text: crate::llm::STREAM_IDLE_TRUNCATION_MARKER.to_string(),
                  thinking: false,
              }).await;
          }
      }
      ```
      **There are two such sites, not one** — checked:
      `task_runner.rs:1626` and `task_runner.rs:2330`, both with a `sink` in
      scope and both with the identical shape. Change **both**. This repo has
      already been bitten by a two-site edit where the test covered only one
      (`handle_tool_call`'s Allow and post-approval execute sites), so the test
      below must exercise whichever site a normal streamed turn goes through,
      and a `command grep -c "STREAM_IDLE_TRUNCATION_MARKER" mur-agent-runtime/src/task_runner.rs`
      must report **3** afterwards (one const use per site, plus the helper).
- [x] Test (spec §5 test 7): an `Interrupted` response reaches
      `task_runner`'s finish path → `resp.text` ends with the marker, the sink
      receives it as one `thinking: false` delta, and the usage JSON carries
      `truncated: true`.
- [x] Test (spec §5 test 13): all **three** `preview.rs` branches
      (`anthropic`/`openai` via `from_env` → `new`, `ollama` via `new`) build a
      client through the shared builder. Assert the same property Task 1's
      guard asserts, so a future edit cannot restore a per-file timeout there.
- [x] Verify and commit:
      ```sh
      cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo $?
      cargo nextest run -p mur-agent-runtime; echo $?
      ```
      Commit: `feat(runtime): mark an interrupted reply in reply, stream and history (#1287)`

**Notes from execution.**

- Both sites patched; `command grep -c STREAM_IDLE_TRUNCATION_MARKER
  mur-agent-runtime/src/task_runner.rs` reports **3** (one per site plus the
  helper), and each site has its own test.
- The agentic path's assertion could not be `ends_with`: that path appends a
  settlement card. The test now splits on the card, requires the marker to be
  last in the answer, and additionally asserts the card names the cause and the
  knob. The card renders by itself as
  `⚠ stopped at stream interrupted (0 iterations) — output may be incomplete ·
  the model stopped sending mid-reply — ask it again; if this repeats on a slow
  local model, raise MUR_LLM_IDLE_TIMEOUT_SECS for that agent`, which is
  `StopKind::StreamInterrupted` paying for itself.
- **`preview.rs` cannot be asserted the way this task imagined.** It lives in
  `mur-core` and only sees `Arc<dyn LlmClient>`, so the HTTP client inside is
  unreachable from there. What its three branches actually depend on is the
  three self-building constructors, so the guard went where the `http` field is
  visible: one test in each of `ollama.rs`, `openai/tests.rs` and
  `anthropic.rs`, each asserting `new()` produces a client with neither a total
  nor a read timeout. That covers both `from_env` branches (they delegate to
  `new`) and the direct `OllamaClient::new` branch.

## Task 5 — whole-workspace verification and the live check

- [ ] `command grep -rn "LLM_REQUEST_TIMEOUT_SECS" mur-agent-runtime/src mur-core/src` → no hits.
- [x] `cargo fmt --all -- --check; echo $?` → `0`.
- [x] `cargo nextest run -p mur-agent-runtime > /tmp/ar.log 2>&1; echo $?` → `0`.
      Record the pass count and compare it with Task 0's recorded count plus
      the tests this plan adds. A count that only went up by less than the
      number of new tests means something stopped being compiled.
- [x] `cargo nextest run -p mur-core > /tmp/core.log 2>&1; echo $?` → `0`.
- [x] `cargo clippy --workspace --all-targets -- -D warnings > /tmp/cw.log 2>&1; echo $?` → `0`.
- [x] Hub, **last**: `cd mur-hub-gui/src-tauri && cargo check > /tmp/hub.log 2>&1; echo $?` → `0`.
      If the worktree lacks `mur-hub-gui/ui/dist`, symlink it from the main
      checkout first and **remove the symlink before committing**.
      `StopReason` is not referenced in the Hub (spec §3.4), so this is a
      regression check, not an expected edit.
- [x] Record the file sizes as *facts*, not as a pass/fail criterion:
      `wc -l mur-agent-runtime/src/llm/{mod.rs,ollama.rs,anthropic.rs,client_builder.rs} mur-agent-runtime/src/llm/openai/mod.rs mur-agent-runtime/src/task_runner.rs`
- [x] Set the spec's Status line to `Implemented in #<PR>`.
- [x] Open PR 2. Body carries: D1–D7 one line each, the §1.1 correction (the
      constants never reached a running agent), the §1.3 verdict from Task 1's
      recorded result, the file-size table above, and the live observations
      below.
- [ ] **Live check.** `./install.sh`, then restart **one** agent —
      `mur agent restart <one agent>`, **not** `--stale`, so the rest of the
      fleet stays on the prior build.
      - [ ] A normal turn through a cloud model still streams and completes.
      - [ ] A local model turn: point an agent at Ollama and send a prompt that
            takes over 60 s to answer. It completes. Before this change the
            runtime had no cutoff, so what this proves is that the new idle
            bound does **not** fire on legitimate slow generation — which is
            the regression this whole change risks.
      - [ ] A cold local model: first token later than 60 s and inside the
            first-chunk bound. Completes.
      - [ ] A turn whose model calls a tool with a large argument payload
            completes with the call intact. This is F1 in the real world.
      - [ ] Kill the model server mid-stream (`pkill ollama` while it is
            emitting). The reply arrives truncated with
            `[output truncated: the model stopped sending]` visible **in
            murmur**, not only in the persisted history, and the turn does not
            error. Record how long it took and confirm it matches the idle
            bound, not some other clock.
      - [ ] `MUR_LLM_IDLE_TIMEOUT_SECS=5` on a restarted agent makes that
            truncation happen at ~5 s — proving the env override reaches the
            running process.
- [ ] **Live check — PARTIALLY DONE, and the central claim was NOT proven live.**
      Recorded exactly as it went, because a live check that quietly becomes a
      claim is worse than no live check.

      **Setup.** `./install.sh` (2.80.0) and the installed
      `mur-agent-runtime` verified to contain the new code (`strings` finds both
      `the model stopped sending` and `MUR_LLM_IDLE_TIMEOUT_SECS`). No existing
      agent was restarted: three throwaway agents were created against fake
      loopback endpoints instead, then purged, so the machine's 26 agents stayed
      on the prior build.

      **What was established.**
      - `mur agent send` drives the **non-stream** path. A path-discriminating
        fake endpoint (valid JSON for `stream:false`, SSE-then-stall for
        `stream:true`) reported `stream=False` and the turn completed in 1 s.
      - **murmur, over the unix socket in this configuration, also drove the
        non-stream path** — same `stream=False`, instant completion, `3/3 tok`.
      - D5's consequence is real and was observed: against an endpoint that
        sends a partial body and then holds the socket, a non-stream call has no
        clock and does not return. What ended it was the dial's own liveness
        check: `agent 'idleprobe' stopped responding — no frame for 90s`. That
        is the user-visible backstop for this surface, and it worked.
      - The global `models.fallback_chain` makes even a single-model agent a
        chain agent (three candidates), confirmed again in the error text.

      **What could NOT be proven live, and why.**
      - The idle bound firing, the partial reply, and the visible marker. Both
        surfaces available here take the non-stream path, so `generate_stream`
        was never entered. `MUR_LLM_IDLE_TIMEOUT_SECS=5` was confirmed present
        in the agent's process environment (`ps eww`), but nothing exercised it.
      - Slow and cold local-model generation. The local Ollama is running with
        **no models installed** (`{"models":[]}`), and pulling a multi-gigabyte
        model was not a call to make unasked.

      So the idle bound is proven by the 16 automated provider tests — which do
      drive the real client code against real sockets, with wall-clock gaps —
      and **not** by a live agent turn. Anyone finishing this should run the
      streaming surface (Hub, or whatever supplies `ctx.notifier` on
      `message/send`) against a stalling endpoint before calling the live check
      done.

      **One observation worth its own look, not a claim.** During the hung
      non-stream turn the dial reported `last: none since the request` — no
      heartbeat arrived in 90 s, while the parent spec's D7 says the runtime
      emits one at least every 30 s during a turn, including model inference. If
      that is a real gap, the dial's backstop is doing work D7 intended the
      heartbeat to make unnecessary. Not investigated here.

- [ ] Tick this task and close #1287 via the PR.

**Recorded facts from Task 5.**

| Check | Result |
|---|---|
| `mur-agent-runtime` nextest | 1117 passed / 6 skipped, exit 0 (1086 on main) |
| `mur-core` nextest | 5992 passed / 23 skipped, exit 0 |
| workspace clippy `--all-targets -D warnings` | exit 0 |
| `cargo fmt --all -- --check` | exit 0 |
| Hub `cargo check` | exit 0 |
| leftover `LLM_REQUEST_TIMEOUT_SECS` | none |

| File | Lines |
|---|---|
| `llm/mod.rs` | 687 |
| `llm/stream_activity.rs` (new) | 228 |
| `llm/stream_idle_tests.rs` (new) | 566 |
| `llm/openai/mod.rs` | 626 |
| `llm/client_builder.rs` | 594 |
| `llm/ollama.rs` | 352 |
| `llm/anthropic.rs` | 1448 (pre-existing violation, 1400 before) |
| `task_runner.rs` | 6335 (pre-existing violation, 6174 before) |

The size check did its job: `llm/mod.rs` reached **901** with `StreamActivity`
inlined, which is a §4 violation this branch created, so it was extracted to its
own module in a separate commit. The two pre-existing violations were declared in
Global Constraints before starting, so they are a recorded deviation and not a
check that failed at the end.

The Hub `ui/dist` symlink was created for the Hub check and **removed before the
commit** — `git status` confirmed clean of it.

## Self-review

- **Spec coverage.** D1 → T1 (three constants deleted, grep check). D2 → T1
  (builder + `client_builder.rs` call site + the two guards). D3 → T2
  (`StreamActivity`) + T3 (three loops, one test each by name). D3a → T2
  (`seen_any` set inside `bounded` before the caller sees the bytes) + T3's two
  F1 guards. D4 → T2 (two bounds, first-vs-idle test). D5 → T1 (no
  `read_timeout`; its absence is asserted) — nothing else to build, since D5 is
  a decision *not* to add a clock. D6 → T3 (per-provider partial assembly,
  including the OpenAI invented-arguments split and Anthropic's `finish_stream`
  flag). D6a → T4 (`mark_stream_interruption` + sink delta + `truncated` flag).
  D7 → T2 (`from_env`, warn-and-keep on a bad value). §1.3 → T1's first two
  steps, which are written to settle it either way. §1.1's three preview
  branches → T4's last test. §3.4 non-exhaustive `match` sweep → T3's first
  step. §3.5 `test-util` → T4's first step. §3.6 is a recorded limitation with
  nothing to build.
- **Cross-task names.** `LLM_CONNECT_TIMEOUT_SECS`, `llm_client_builder` (T1)
  are used verbatim in T4's preview test. `StreamActivity::{from_env,
  bounded}`, `LLM_FIRST_CHUNK_TIMEOUT_SECS`, `LLM_STREAM_IDLE_TIMEOUT_SECS`
  (T2) are used verbatim in T3. `StopReason::Interrupted` and
  `STREAM_IDLE_TRUNCATION_MARKER` (T3) are used verbatim in T4.
  `finish_stream`'s new arity (T3) is updated at its one call site and its one
  existing test in the same task.
- **No placeholders.** Every step names a file, and every code block is
  complete enough to paste. The three places that are deliberately not
  literal code are marked as such and say what to copy instead: T1's proxy
  test body (copy `proxy_isolation_tests`), T3's per-provider test harness
  (copy the same), and T1's builder-property assertion, which names a fallback
  in case `reqwest::Client`'s `Debug` does not expose the field.
- **Known soft spots, each with an instruction rather than a guess.**
  (a) Whether `reqwest::Client: Debug` prints `connect_timeout` — T1 says what
  to do if not. (b) `task_runner`'s finish sites — resolved before
  handoff rather than left as a check: there are exactly two, at 1626 and
  2330, and T4 names both plus a count assertion. (c) Whether Task 0's moved test module needs import
  fixes — T0 says to let the compiler say so rather than pre-editing.
- **What this plan deliberately does not do.** Split `anthropic.rs` (1400) or
  `task_runner.rs` (6174). Both are pre-existing §4 violations and both are
  refactors several times the size of this feature. Stated in Global
  Constraints so it is a recorded deviation rather than a check that fails at
  the end.
