# LLM clients: bound idleness, not generation

**Status:** Drafted 2026-09-13; revised the same day after review — six findings, all six valid, none rebutted. The decorator seam of the first draft is gone (§7 F1). Awaiting re-review.
**Scope:** `llm/mod.rs` (shared client builder, `StopReason`, one marker constant, the shared activity helper), `llm/client_builder.rs` (the single place the runtime's HTTP client is built), `llm/{ollama,openai,anthropic}.rs` (delete three constants; each SSE loop awaits through the helper), `task_runner.rs` (mark an interrupted reply the way `MaxTokens` is already marked), the workspace `Cargo.toml` (tokio `test-util`, dev-only), and `mur-core/src/cmd/agent_companion/preview.rs` (all three of its timeout-bearing branches). No wire-protocol change. No new YAML keys; two env overrides.
**Parent:** `docs/superpowers/specs/2026-09-12-execution-limits-design.md` — applies its D3 ("attended runs have no hard stops") and D7 ("liveness is heartbeats, not a bigger read timeout") to the LLM transport, the last layer the redesign did not reach.
**Issue:** #1287, filed by the sibling-limit scan in `docs/superpowers/specs/2026-09-12-bash-yield-not-kill-design.md` §1.

## 1. Problem

The issue reports that `LLM_REQUEST_TIMEOUT_SECS = 60` on the Ollama and
OpenAI clients covers the whole request including the streamed body, so a
slow local model producing a long answer is cut off mid-stream.

That is true of the constant. It is not true of the agent runtime, and the
difference matters more than the reported bug.

### 1.1 The constants do not reach a running agent

```rust
// mur-agent-runtime/src/llm/ollama.rs:8   → 60
// mur-agent-runtime/src/llm/openai.rs:18  → 60
// mur-agent-runtime/src/llm/anthropic.rs:48 → 180
```

Each is applied in that client's *self-building* constructor (`new`,
`from_secret_string`, `from_agent_credentials`). The runtime does not use
those. It builds one HTTP client itself and injects it:

```rust
// mur-agent-runtime/src/llm/client_builder.rs:189
let guarded_http = reqwest::ClientBuilder::new()
    .dns_resolver(std::sync::Arc::new(host_guard))
    .build()
```

and hands it to `OllamaClient::with_http_client`,
`OpenAiClient::from_secret_string_with_http`,
`AnthropicClient::from_agent_credentials_with_http`,
`ClaudeClient::from_entry`, `CodexClient::from_entry` — every one of which
stores the client verbatim and adds nothing.

So inside a running agent there is **no 60-second cutoff**. The reported
symptom cannot occur there. The 60 seconds does bite in one production place, and in all three of its
branches: `mur-core/src/cmd/agent_companion/preview.rs` builds
`AnthropicClient::from_env` (line 321), `OpenAiClient::from_env` (329) and
`OllamaClient::new` (339). Both `from_env` calls delegate to that client's own
`new` (`openai.rs:103`, `anthropic.rs:226`), so each carries its file's
constant.

### 1.2 The real defect is the opposite one

That injected client has **no timeouts at all** — no total, no read, not even
`connect_timeout`. An endpoint that accepts the connection and then says
nothing holds the turn open indefinitely. That is a direct violation of the
parent spec's D7: liveness must be bounded, and here it is not bounded at
all. It is also worse than being cut off, because a killed request at least
reports a failure the fallback chain can act on.

### 1.3 A third divergence at the same line, with a security consequence

`llm/mod.rs:23` is the shared builder:

```rust
pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}
```

`.no_proxy()` is deliberate and load-bearing. A test guards it
(`llm_client_builder_ignores_ambient_http_proxy`, `llm/mod.rs:577`) with the
reason: "the per-server egress proxy / a debug cc-proxy never captures LLM
traffic."

`client_builder.rs:189` does not go through that builder, so the runtime's
own LLM client **does not have `.no_proxy()`**. With `HTTP_PROXY` or
`HTTPS_PROXY` set in the agent's environment, its LLM traffic is routed
through that proxy.

The consequence is not only interception. `HostGuard` is installed as a
`dns_resolver` (`sandbox/reqwest_guard.rs:126`) and it is the **only** host
enforcement for the runtime's own egress — the OS sandbox layer can restrict
ports, not hosts, and `sandbox/egress_proxy.rs` governs child processes, not
this client. When reqwest routes through a proxy it resolves the *proxy*
host, so the allowlist in `entitlements.network.outbound.allow_hosts` is
never consulted for the real destination.

This is stated here as what the code shows. §5 test 7 is what will prove or
disprove the bypass; the spec does not claim it as established until that
test runs.

### 1.4 Which calls stream, and which do not

`task_runner.rs:1621` picks per call:

```rust
Some(s) => client.generate_stream(req, s.clone()).await,
None    => client.generate(req).await,
```

A murmur or Hub turn has a sink and streams. `mur agent send`, the companion
outbox (`companion/outbox/generate.rs:110`) and `companion/i18n.rs:84` have
no sink and use the whole-response call. The two cases need different
treatment and §2 D4 says why: a stream that stops emitting is observably
dead, while a non-stream request in flight is indistinguishable from a
server that is still thinking.

## 2. Decisions

| # | Decision | Rejected alternative |
|---|---|---|
| D1 | **No total request timeout anywhere.** Delete all three `LLM_REQUEST_TIMEOUT_SECS`. A body that is still arriving is live work and no clock may end it. | Raise 60 to 600. The parent spec's D6 rejected exactly this shape: the next model that takes 20 minutes hits it again. |
| D2 | **One builder, one place, and `connect_timeout` is the only clock on it.** `client_builder.rs` builds its guarded client through `llm_client_builder()`. No `read_timeout`: reqwest polls that timer while awaiting the response head (`async_impl/client.rs:3053`), so any value at all is a hard stop on a model that thinks before its first byte — the thing D5 exists to refuse. | (a) Fix the three constants in place: the two paths have already drifted into three differences (timeouts, proxy, which one production uses); a fourth is a matter of time. (b) Keep a generous `read_timeout` as a transport backstop — the first draft did, and it contradicted its own D5 and the parent's attended-run rule (§7 F5). |
| D3 | **Liveness is measured where activity is observable: the provider's own SSE loop.** One shared helper (`StreamActivity`, §3.2) owns the timing rule; each of the three loops that parse an event stream awaits its next chunk through it. `ClaudeClient` and `CodexClient` wrap `AnthropicClient` and `OpenAiClient`, so they inherit it. | A decorator wrapping every client, timing the gaps between `StreamDelta`s — the first draft's design. It cannot work: tool-call fragments emit **no** delta (`openai.rs:518` accumulates into `partial`, `anthropic.rs:533` returns `None`), so a model streaming a large tool invocation looks idle and would be aborted mid-call. §7 F1. |
| D3a | **Activity means any parsed event, visible or not** — text, thinking, a tool-argument fragment, a usage or control frame. The helper is pinged before the event is interpreted, so a fragment that produces nothing user-facing still counts. | Count only what reaches the sink. That is the same mistake as D3's rejected alternative, one layer down. |
| D4 | **Two bounds, because one number cannot serve both ends.** A *first-delta* bound covers cold start (a local model loading weights and evaluating a long prompt: minutes). A *between-delta* bound covers a stream that has gone quiet (sub-second in normal operation, so a tight bound is safe). Defaults 300 s and 120 s. | One idle bound. It must be at least the cold-start figure, which makes a dead mid-stream hang for five minutes — the bound stops meaning anything. |
| D5 | **Non-stream calls are governed by the turn's deadline and by cancellation, not by a socket clock.** Before the first byte, "black-holed" and "still thinking" are indistinguishable, so the parent spec's D3 applies: an attended run has no hard stop and the user cancels it (murmur's Ctrl-C already does); an unattended one is bounded by its deadline. A socket that is silent forever in an attended run is therefore held forever, and that is the deliberate answer, not an oversight. | Apply a tight idle bound, or any `read_timeout`, to non-stream calls. That treats "the server is thinking" as "the connection died" — the same mistake as the 60 seconds, relocated. |
| D6 | **An interrupted stream returns whatever the provider had already assembled**, as `Ok` with `StopReason::Interrupted`. The provider owns that state, so the answer text is the text it had accumulated (never the thinking buffer — Anthropic's `thinking_delta` arm deliberately does not touch `acc.text`) and the completed tool calls come along instead of being discarded. Nothing usable assembled → `LlmError::Timeout`. | Assemble the partial in a decorator from forwarded deltas. It would have concatenated hidden reasoning into the answer (§7 F2) and thrown away the tool call the model was mid-way through emitting (§7 F1). |
| D6a | **The marker is applied in `task_runner`, next to the `MaxTokens` one, and goes to the sink too.** `mark_max_tokens_truncation` + a sink delta already exist for exactly this at `task_runner.rs:2329-2339`; `Interrupted` gets the parallel treatment. | Have the provider append the marker to its own text. The streamed UI would then show an apparently complete answer while only the persisted copy carried the marker (§7 F3), and the marking rule would live in two places. |
| D7 | **The bounds are constants with env overrides**, `MUR_LLM_FIRST_DELTA_TIMEOUT_SECS` and `MUR_LLM_IDLE_TIMEOUT_SECS`. | A profile or `config.yaml` key. This is a number a local-model user tunes once on one machine; plumbing it through `AgentProfile`, the Hub and `mur agent` costs more than it buys. |

## 3. Design

### 3.1 The builder

```rust
// llm/mod.rs
/// Time allowed to establish a TCP connection to an LLM endpoint. The only
/// clock on the client: see D2 for why there is no `read_timeout`.
pub(crate) const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;

pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
}
```

`reqwest::ClientBuilder::read_timeout` exists (0.12.28,
`async_impl/client.rs:1453`) and is deliberately **not** used. Read from the
vendored source rather than the docs: its timer is polled in
`PendingRequest::poll` (`async_impl/client.rs:3053`) *and* again per body read
(`async_impl/body.rs:397`). Because the first of those runs while the response
head is still outstanding, any value becomes a hard ceiling on server think
time — which is what the first draft got wrong (§7 F5). It is also
client-wide with no per-request form, so it could not have been applied to
streams only.

`client_builder.rs` becomes:

```rust
let guarded_http = crate::llm::llm_client_builder()
    .dns_resolver(std::sync::Arc::new(host_guard))
    .build()
    .context("failed to build guarded HTTP client")
    .map_err(GuardedHttpBuildError)?;
```

The three per-file `LLM_REQUEST_TIMEOUT_SECS` and the three per-file
`LLM_CONNECT_TIMEOUT_SECS` are deleted; the self-building constructors call
`llm_client_builder()` and add nothing.

### 3.2 The activity helper

```rust
// llm/mod.rs
/// Bounds how long a stream may be silent — not how long it may run.
///
/// Two bounds because one number cannot serve both ends (D4): the wait for
/// the FIRST event covers a cold local model loading weights and evaluating
/// a long prompt, while the wait between events covers a stream that has
/// gone quiet, which in normal operation is sub-second.
pub(crate) struct StreamActivity { /* bounds + whether anything arrived yet */ }

impl StreamActivity {
    pub(crate) fn from_env_or_defaults() -> Self;

    /// Await the next chunk, bounded. `Ok(None)` = the stream ended normally;
    /// `Err(Idle)` = the bound elapsed. Every call that returns a chunk also
    /// records the activity, so the caller cannot forget to.
    pub(crate) async fn next<S>(&mut self, s: &mut S) -> Result<Option<S::Item>, Idle>;
}
```

Each provider's loop changes shape from

```rust
while let Some(chunk) = stream.next().await { ... }
```

to awaiting through the helper, and on `Err(Idle)` breaking out to the
assembly step below. The ping happens inside `next`, before the caller
interprets the chunk, which is what makes D3a hold: an SSE frame carrying
only a tool-argument fragment, or only a usage update, counts as life even
though nothing reaches the sink.

Three loops own this: `ollama.rs`, `openai.rs`, `anthropic.rs`.
`ClaudeClient` wraps `AnthropicClient` and `CodexClient` wraps `OpenAiClient`
(`claude.rs:22`, `codex.rs:28`), so both inherit it with no change — and a
missed edit in one of the three would be inherited too, which is why §5 test
4 runs against each of the three by name rather than against one of them.

### 3.3 What an interruption returns

At `Err(Idle)` the provider already holds everything it has parsed. It
returns:

- **Usable content** — non-empty answer text, or at least one *complete* tool
  call — → `Ok(LlmResponse { text, tool_calls, stop_reason: Interrupted,
  output_tokens: 0, .. })`.
- **Nothing usable** → `Err(LlmError::Timeout)`. `classify` maps that to
  `RetryThenAdvance`, which is safe precisely because no answer text reached
  the sink. Thinking deltas may have; those are a transient indicator, and
  re-running is the right call when there is no answer to duplicate.

A tool call interrupted **mid-arguments** is dropped, not guessed: the
argument JSON is incomplete and Anthropic's `content_block_stop` arm already
commits only closed blocks. The reply then carries whatever text preceded it
plus the marker, so the model is told plainly that its call did not happen
rather than being handed invented arguments.

### 3.4 The new stop reason and marker

```rust
pub enum StopReason { EndTurn, ToolUse, MaxTokens, Interrupted }

/// Appended when a stream stopped sending and the partial reply was kept.
/// Same rule as MAX_TOKENS_TRUNCATION_MARKER (#715): a truncated reply must
/// never look complete.
pub const STREAM_IDLE_TRUNCATION_MARKER: &str =
    "\n\n[output truncated: the model stopped sending]";
```

`task_runner.rs` applies it beside the existing `MaxTokens` handling
(`task_runner.rs:2329-2339`): mark the response, and send the marker to the
sink as one `thinking: false` delta so the live UI shows the truncation too.
One rule, one place, already-proven shape.

Adding a variant makes every `match` on `StopReason` non-exhaustive, which is
the point: each site is a decision about whether interrupted behaves like
`MaxTokens` (truncated) or `EndTurn` (complete).

Checked rather than assumed: `StopReason` appears only inside
`mur-agent-runtime` (`task_runner.rs`, `supervisor_runner.rs`,
`protocol/methods/model_set.rs`, `llm/*`). It is **not** referenced in
`mur-core` or in the workspace-excluded `mur-hub-gui`, so the usual
excluded-crate hazard does not apply and the Hub needs no change. The
`--all-targets` build is still what proves it, because the bin target's own
`match` sites are the ones a `--lib` check misses (#1289).

### 3.5 Test clock

`tokio::time::pause` needs the `test-util` feature, which the workspace
manifest does not enable (`Cargo.toml:40`). It is added as a **dev-only**
feature so no production build gains it:

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
```

Named here because the first draft mandated a paused clock while excluding
the manifest from its scope (§7 F4).

### 3.6 Known limitation, recorded rather than papered over

An interrupted stream reports `output_tokens: 0`. The real count is not
recoverable — providers send usage in the final SSE frame, which by
definition never arrived. The turn's cost is therefore understated by
whatever the partial text cost. The alternative is to estimate from
character count, which invents a number that then flows into the ledger and
the budget guards, and a wrong cost figure in a budget check is worse than a
missing one.

## 4. Errors

| Situation | Result |
|---|---|
| TCP connect refused / DNS fails / `connect_timeout` elapses | `LlmError::Connect` (unchanged) — `RetryThenAdvance` |
| Non-stream call, server thinks for 20 minutes before the head | succeeds. No `read_timeout` exists to cut it (D2) |
| Non-stream call, socket accepted then silent forever, attended | held open until the user cancels — the deliberate consequence of D5 |
| Non-stream call, same, unattended | ends with the task's deadline, not with a socket clock |
| Stream, no event at all inside the first-event bound | `LlmError::Timeout`; nothing reached the sink, so the chain retries safely |
| Stream, gap between events exceeds the idle bound, text or a complete tool call assembled | `Ok` + `StopReason::Interrupted`; `task_runner` marks it and sends the marker to the sink; the chain does **not** retry, so no duplicated prose |
| Stream, gap exceeds the bound with only thinking or a half-finished tool call | `LlmError::Timeout` — nothing usable, and no answer text was delivered to duplicate |
| Stream carrying only tool-argument fragments for minutes | alive. D3a counts the fragments (`openai.rs:518`, `anthropic.rs:533`), which is the bug the decorator design would have had |
| Stream carrying only `thinking` for minutes | alive, same rule |
| Env override unparseable | fall back to the constant and `warn!` once — a typo in an env var must not disable the bound |
| `HTTP_PROXY` set in the agent environment | ignored, as on the other path (D2) |

## 5. Tests

1. `llm_client_builder` keeps `.no_proxy()`, sets `connect_timeout`, and sets **no** `read_timeout` — the last is the regression guard for §7 F5, asserted by building a client against a listener that accepts and then stalls longer than any former backstop.
2. The guarded client built by `build_bare_client` carries the same properties — the regression that lets the two paths drift again.
3. No `LLM_REQUEST_TIMEOUT_SECS` remains anywhere: `grep` over `mur-agent-runtime/src` and `mur-core/src`.
4. **Per provider, named individually** (`ollama`, `openai`, `anthropic`): a stream whose gaps are inside the bound delivers every delta, preserves `EndTurn`, and adds no marker. Three tests, because D3's rule lives in three loops.
5. First event never arrives: `Err(LlmError::Timeout)` and the sink received nothing.
6. Stream stops after three text deltas: `Ok`, `stop_reason == Interrupted`, returned text is those three, and the sink received exactly **three** deltas from the client. The fourth (the marker) is asserted in test 7, at the layer that actually sends it.
7. `task_runner` handling of `Interrupted`: the response text gains `STREAM_IDLE_TRUNCATION_MARKER` and the sink receives it as one `thinking: false` delta — the same assertions the `MaxTokens` path already carries.
8. **The tool-fragment case, F1's regression guard.** An OpenAI-shaped stream that emits one text delta then only `tool_calls` fragments for longer than the idle bound completes normally with its tool call intact. The same for an Anthropic-shaped stream of `input_json_delta`.
9. Interrupted mid-arguments: the incomplete tool call is absent from `tool_calls`, and the reply is `Interrupted` with the preceding text (or `Timeout` if there was none).
10. `thinking: true` events keep a stream alive, and the thinking text does **not** appear in the returned answer — F2's regression guard.
11. Env overrides parsed; an unparseable value falls back to the constant and warns.
12. **The proxy claim from §1.3.** A restricted-mode agent whose `allow_hosts` excludes the target, with `HTTP_PROXY` pointed at a local listener: assert the request does not reach the proxy listener. Run it against the pre-fix builder first and record what happens — if it passes before the fix, §1.3's bypass is wrong and §7 says so instead of the claim standing.
13. All **three** `preview.rs` branches (`anthropic`/`openai` via `from_env` → `new`, and `ollama` via `new`) build through the shared builder. The first draft named only the Ollama one (§7 F6).

Every stream test drives a `tokio::time::pause`d clock (§3.5), not a real sleep.

## 6. Out of scope

- Per-model or per-profile bounds. D7.
- Retrying an interrupted stream from where it stopped. No provider offers resumption, and re-prompting with the partial text as context is a different feature.
- Recovering a tool call that was interrupted mid-arguments. §3.3.
- `protocol/http_mcp_client.rs:64`, which builds its own client. The MCP call timeout is governed by the parent spec's D6 and was reviewed there.
- Recovering `output_tokens` for an interrupted stream. §3.6.

## 7. Review log

### Round 1, 2026-09-13 — six findings, six valid, none rebutted

Each was checked against the source before being accepted; the verification
is named so a re-reviewer can repeat it rather than trust it.

- **F1 (critical) — the decorator would kill live tool-call streams.** Upheld.
  `openai.rs:518` accumulates `tool_calls` fragments into `partial` and sends
  nothing; `anthropic.rs:533` handles `input_json_delta` and returns `None`.
  A model streaming a large tool invocation emits zero deltas, so the
  delta-timing decorator saw silence — and with D6 as first written it would
  have returned the preceding narration as a finished answer and dropped the
  call. This removed the decorator entirely: D3 now puts the rule in the three
  SSE loops, and D3a makes any parsed event count. The reviewer's two options
  were an internal stream-event type or timing the body chunks; the second is
  what this takes, because it needs no change to `LlmClient`'s sink type and
  the provider already holds the partial state D6 now returns.
- **F2 (important) — hidden reasoning would have leaked into the answer.**
  Upheld. `StreamDelta` carries `thinking` (`llm/mod.rs:366-372`) and
  Anthropic's `thinking_delta` arm deliberately never touches `acc.text`,
  while `text_delta` does. A decorator concatenating every forwarded delta
  would have put reasoning in the reply. Dissolved by F1's fix: the provider
  returns its own already-correct text buffer. Test 10 guards it.
- **F3 (important) — the marker never reached the live sink.** Upheld.
  `task_runner.rs:2329-2339` already sends `MAX_TOKENS_TRUNCATION_MARKER` to
  the sink, with a comment naming all three destinations (reply, streamed
  output, persisted history). Draft test 6 asserting three sink deltas would
  have left a streaming UI showing an apparently complete answer. Of the
  reviewer's two options — assert a fourth delta, or specify a downstream
  handler — this takes the second (D6a), because it reuses the existing
  `MaxTokens` code path instead of teaching a second layer the marking rule.
  Test 6 keeps its three-delta assertion at the client layer and test 7
  asserts the fourth where it is actually sent.
- **F4 (important) — the paused clock needed an unplanned manifest change.**
  Upheld. `Cargo.toml:40` lists tokio's features and `test-util` is absent
  from the whole repo. Added as a dev-only feature and the manifest is now in
  Scope (§3.5).
- **F5 (critical) — the 600 s read backstop contradicted D5 and the parent.**
  Upheld, and it was self-inflicted: the draft cited
  `async_impl/client.rs:3053` — the line proving `read_timeout` is polled
  while the response head is outstanding — as evidence the backstop was
  *sound*, when it is the evidence that it is a hard ceiling on think time.
  The backstop is gone; `connect_timeout` is the only clock on the client.
  The consequence the reviewer asked to be made explicit is now stated as a
  decision rather than buried: an attended non-stream call against a silent
  socket is held open until the user cancels.
- **F6 (minor) — preview coverage understated the affected constructors.**
  Upheld. All three `preview.rs` branches use timeout-bearing constructors,
  and both `from_env` paths delegate to `new`. §1.1 and test 13 now name all
  three.

Net effect: the one architectural seam the review called correct — a single
provider-agnostic place for the rule — survives as the shared
`StreamActivity` helper, but the *observation point* moved from the sink to
the byte stream, because that is the only place where invisible protocol
activity is visible at all.
