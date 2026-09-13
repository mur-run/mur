# LLM clients: bound idleness, not generation

**Status:** Drafted 2026-09-13. Awaiting review.
**Scope:** `mur-agent-runtime` only — `llm/mod.rs` (shared client builder, `StopReason`, one marker constant), `llm/client_builder.rs` (the single place the runtime's HTTP client is built, plus one new decorator), `llm/{ollama,openai,anthropic}.rs` (delete three constants), and `mur-core/src/cmd/agent_companion/preview.rs` (the one production caller of a timeout-bearing constructor). No wire-protocol change. No new YAML keys; two env overrides.
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
symptom cannot occur there. The 60 seconds does bite in exactly one
production place: `mur-core/src/cmd/agent_companion/preview.rs:339` calls
`OllamaClient::new`.

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
| D2 | **One builder, one place.** `client_builder.rs` builds its guarded client through `llm_client_builder()`. That builder gains `connect_timeout` and a deliberately generous `read_timeout` transport backstop. Every client keeps taking an injected client and adding nothing. | Fix the three constants in place. The two paths have already drifted into three separate differences (timeouts, proxy, and which one production uses); a fourth is a matter of time. |
| D3 | **Stream liveness is the gap between deltas, measured once.** A decorator in `client_builder.rs` — beside the existing `EndpointNamed`, wrapping every provider — passes its own channel to the inner client, forwards each delta to the real sink, and times the gaps. Provider-agnostic, one implementation. | Add an idle guard to each client's own stream loop. Three copies of one rule, in the three clients that own a stream loop (`ClaudeClient` wraps `AnthropicClient` and `CodexClient` wraps `OpenAiClient`, so those two inherit whatever the wrapped client does — and would silently inherit a missed edit too). |
| D4 | **Two bounds, because one number cannot serve both ends.** A *first-delta* bound covers cold start (a local model loading weights and evaluating a long prompt: minutes). A *between-delta* bound covers a stream that has gone quiet (sub-second in normal operation, so a tight bound is safe). Defaults 300 s and 120 s. | One idle bound. It must be at least the cold-start figure, which makes a dead mid-stream hang for five minutes — the bound stops meaning anything. |
| D5 | **Non-stream calls are governed by the turn's deadline, not by a socket clock.** Nothing observable distinguishes thinking from hanging, so the parent spec's D3 applies: an attended run has no hard stop and the user cancels; an unattended one is bounded by its deadline. The `read_timeout` backstop from D2 is set far above any plausible think time and exists only so a black-holed socket is not immortal. | Apply the tight idle bound to non-stream calls too. That treats "the server is thinking" as "the connection died", which is the same mistake as the 60 seconds, just relocated. |
| D6 | **An interrupted stream that already delivered text returns that text, marked.** The decorator assembles what it forwarded, returns `Ok` with a new `StopReason::Interrupted` and appends a visible marker. Only an abort with **zero** deltas delivered returns `LlmError::Timeout`. | Always return `LlmError::Timeout`. `classify` maps `Timeout` to `RetryThenAdvance`, so the fallback chain would re-run the turn and the sink would receive the same prose twice. Partial output already has a precedent for being kept and marked (`MAX_TOKENS_TRUNCATION_MARKER`, issue #715). |
| D7 | **The bounds are constants with env overrides**, `MUR_LLM_FIRST_DELTA_TIMEOUT_SECS` and `MUR_LLM_IDLE_TIMEOUT_SECS`. | A profile or `config.yaml` key. This is a number a local-model user tunes once on one machine; plumbing it through `AgentProfile`, the Hub and `mur agent` costs more than it buys. |

## 3. Design

### 3.1 The builder

```rust
// llm/mod.rs
/// Time allowed to establish a TCP connection to an LLM endpoint.
pub(crate) const LLM_CONNECT_TIMEOUT_SECS: u64 = 10;
/// Transport backstop: complete silence on an established socket for this
/// long is a dead connection, not a slow model. Deliberately far above any
/// plausible server think time — D5 makes the turn deadline the real
/// governor, and for streams D3's decorator fires long before this does.
pub(crate) const LLM_READ_BACKSTOP_SECS: u64 = 600;

pub(crate) fn llm_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(LLM_CONNECT_TIMEOUT_SECS))
        .read_timeout(Duration::from_secs(LLM_READ_BACKSTOP_SECS))
}
```

`read_timeout` exists on `reqwest::ClientBuilder` (0.12.28,
`async_impl/client.rs:1453`) and is **client-wide only** — there is no
per-request form. That absence is why D3 is a decorator rather than a
per-call reqwest setting.

What it covers, read from the vendored source rather than the docs: it is
polled in `PendingRequest::poll` (`async_impl/client.rs:3056`) and again per
body read (`async_impl/body.rs:397`), so it bounds the wait for the response
head as well as the gaps between body chunks. That is what makes it a valid
backstop for a server that accepts the connection and then sends nothing at
all.

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
`llm_client_builder()` with nothing added.

### 3.2 The idle decorator

`IdleGuarded` wraps the client the same way `EndpointNamed` already does, and
nests inside it so an interruption is not decorated with the auth paragraph:

```rust
build_client_from_entry
  └── EndpointNamed          (names the endpoint on Auth)
        └── IdleGuarded      (bounds stream idleness)
              └── provider client
```

`generate` passes straight through — D5.

`generate_stream` creates an inner channel, spawns the inner call against it,
and runs a forwarding loop:

- `tokio::time::timeout(first_delta_bound, inner_rx.recv())` for the first
  delta; `timeout(idle_bound, inner_rx.recv())` for every one after.
- Each delta is forwarded to the caller's sink and its text appended to a
  local buffer. `thinking` deltas count as liveness: a reasoning model that
  thinks for two minutes before its first visible token is alive.
- On elapse, the inner future is aborted. Its process group is not a concern
  here — this is an HTTP request, and dropping it closes the connection.
- Zero deltas seen → `Err(LlmError::Timeout)`.
- One or more seen → `Ok(LlmResponse { text: buffer + MARKER, stop_reason:
  StopReason::Interrupted, output_tokens: 0, .. })`.

### 3.3 The new stop reason and marker

```rust
pub enum StopReason { EndTurn, ToolUse, MaxTokens, Interrupted }

/// Appended when a stream stopped emitting and the partial text was kept.
/// Same rule as MAX_TOKENS_TRUNCATION_MARKER (#715): a truncated reply must
/// never look complete.
pub const STREAM_IDLE_TRUNCATION_MARKER: &str =
    "\n\n[output truncated: the model stopped sending]";
```

Adding a variant makes every `match` on `StopReason` non-exhaustive, which is
the point: each site is a decision about whether interrupted behaves like
`MaxTokens` (truncated) or `EndTurn` (complete).

Checked rather than assumed: `StopReason` appears only inside
`mur-agent-runtime` (`task_runner.rs`, `supervisor_runner.rs`,
`protocol/methods/model_set.rs`, `llm/*`). It is **not** referenced in
`mur-core` or in the workspace-excluded `mur-hub-gui`, so the usual
excluded-crate hazard does not apply here and the Hub needs no change. The
`--all-targets` build is still what proves it, because the bin target's own
`match` sites are the ones a `--lib` check misses (#1289).

### 3.4 Known limitation, recorded rather than papered over

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
| TCP connect refused / DNS fails | `LlmError::Connect` (unchanged) — `RetryThenAdvance` |
| Connect accepted, no bytes at all, `connect_timeout` already passed | `read_timeout` backstop fires at 600 s → reqwest error → `LlmError::Timeout` |
| Stream, no first delta inside the first-delta bound | `LlmError::Timeout`, nothing delivered, chain retries safely |
| Stream, delta gap exceeds the idle bound | `Ok` + partial text + marker + `StopReason::Interrupted`; the chain does **not** retry, so no duplicated prose |
| Non-stream call, server thinks for 20 minutes | succeeds; bounded only by the turn deadline (D5) |
| Non-stream call, socket silent for 600 s | `LlmError::Timeout` |
| Env override unparseable | fall back to the constant and `warn!` once — a typo in an env var must not disable the bound |
| `HTTP_PROXY` set in the agent environment | ignored, as on the other path (D2) |

## 5. Tests

1. `llm_client_builder` sets connect and read timeouts, and keeps `.no_proxy()`.
2. The guarded client built by `build_bare_client` carries the same three — the regression that lets the two paths drift again.
3. No `LLM_REQUEST_TIMEOUT_SECS` remains anywhere: `grep` over `mur-agent-runtime/src` and `mur-core/src`.
4. Idle decorator, stream with a gap shorter than the bound: all deltas forwarded, `EndTurn` preserved, no marker.
5. Idle decorator, first delta never arrives: `Err(LlmError::Timeout)`, sink received nothing.
6. Idle decorator, stream stops after three deltas: `Ok`, text is those three plus the marker, `stop_reason == Interrupted`, and the sink received exactly three deltas — proving the caller is not sent the marker twice.
7. **The proxy claim from §1.3.** A restricted-mode agent whose `allow_hosts` does not include the target, with `HTTP_PROXY` pointed at a local listener: assert the request does not reach the proxy listener. Run this test against the pre-fix builder first and record what it does — if it passes before the fix, §1.3's bypass is wrong and this spec says so in §7 rather than keeping the claim.
8. `thinking: true` deltas keep a stream alive: a reasoning model emitting only thinking for longer than the idle bound is not interrupted.
9. Env override parsed; an unparseable value falls back to the constant.
10. Companion preview (`preview.rs:339`) builds through the shared builder — the one production caller that really had a 60 s cutoff.

Every stream test drives a `tokio::time::pause`d clock, not a real sleep.

## 6. Out of scope

- Per-model or per-profile bounds. D7.
- Retrying an interrupted stream from where it stopped. No provider offers resumption, and re-prompting with the partial text as context is a different feature.
- `protocol/http_mcp_client.rs:64`, which builds its own client. The MCP call timeout is governed by the parent spec's D6 and was reviewed there.
- Recovering `output_tokens` for an interrupted stream. §3.4.

## 7. Review log

Empty — awaiting first review.
