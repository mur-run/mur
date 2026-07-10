# Deep-Research Fetch Content Budget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop deep-research worker turns from overflowing the LLM context by capping the `research-gateway` `fetch` tool's returned page text to a configurable character budget — so a single large page no longer blows past the model's context window.

**Architecture:** The gateway's `fetch` returns `html_to_text(body)` — the full extracted text, bounded only by the 5MB body cap. That full text becomes a `ToolResultEntry.content` the runtime feeds verbatim into the worker's LLM context (`cap_step_output` truncates only the progress *notification*, not the content). A few multi-hundred-KB fetches overflow claude_haiku, the turn fails with `anthropic 400: "prompt is too long"`, never reaches `Completed`, and the fleet can't converge. Fix at the domain layer: a `max_fetch_chars` config knob truncates the returned text with an explicit marker. Search results (short title/url/snippet) are unaffected.

**Tech Stack:** Rust (edition 2024), `mur-research-gateway` only. Existing deps only.

## Root Cause (empirically pinned, 2026-07-10 live fleet run)

After the egress/HITL/G1/G2/G3 fixes, a live fleet iteration showed
`search ok=true ×4` and `fetch ok=true ×5` — the web layer works — yet `s1`
still failed and no worker reply landed in the channel. Worker stderr:

```
anthropic non-2xx status=400 Bad Request
body={"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long ...
```

The worker accumulates each fetched page's full text into its conversation
history. `mur-research-gateway/src/fetcher.rs:8` caps the fetched *body* at
`MAX_BODY_BYTES = 5 * 1024 * 1024` (5 MB) but never caps the extracted
`FetchResult.text`; `mur-agent-runtime/src/task_runner.rs:1168` returns the
tool `output` as `ToolResultEntry.content` verbatim (only the notification is
capped, via `cap_step_output`). So one large page can inject ~1M+ tokens →
context overflow → `TaskOutcome::Failed` → no `channel/delegate` self-append
→ empty delegate reply → `dag.rs` `exit_code = 1` → fleet aborts.

This is a content-budget bug, not a sandbox/egress gap (those are all fixed).

## Global Constraints

- **No hardcoded values:** the budget is a named const `DEFAULT_MAX_FETCH_CHARS` plus a config field `max_fetch_chars`, overridable by env `MUR_RESEARCH_MAX_FETCH_CHARS` and YAML `max_fetch_chars`, following the EXACT idiom of the existing `search_limit` knob (const + `ENV_*` + `GatewayConfigYaml` field + `env_usize().or(raw).unwrap_or(DEFAULT)`).
- **Opt-out sentinel:** `max_fetch_chars == 0` means "no cap" (return full text) — so an operator can restore the old behavior. Document it.
- **Truncate on a char boundary** (never split a UTF-8 codepoint) and append a visible marker so the model knows the text was cut.
- **Search is untouched:** `search` returns short title/url/snippet triples — only `fetch` (tier-1 and rendered) text is capped.
- **Default value:** `DEFAULT_MAX_FETCH_CHARS = 50_000` (≈12–15k tokens/fetch; ~10 fetches fit claude_haiku's 200k window with room for reasoning). It is a starting default, tunable via config.
- Single source file ≤ 800 lines; `cargo clippy --workspace -- -D warnings` + `cargo fmt --check` clean.
- Test: `export ORT_STRATEGY=download`; `cargo test -p mur-research-gateway`.

---

### Task 1: `cap_text` helper in `fetcher.rs`

**Files:**
- Modify: `mur-research-gateway/src/fetcher.rs` (add a `pub(crate) fn cap_text` + tests)

**Interfaces:**
- Produces: `pub(crate) fn cap_text(text: &str, max_chars: usize) -> String`. `max_chars == 0` returns the text unchanged (opt-out); otherwise returns at most `max_chars` chars, and when it truncates, appends `\n…[truncated N chars]` where N is the number of chars dropped. Task 2 (server) consumes it.

- [ ] **Step 1: Write the failing tests** (append to the `#[cfg(test)] mod tests` in `fetcher.rs`)

```rust
    #[test]
    fn cap_text_under_limit_is_unchanged() {
        assert_eq!(cap_text("hello world", 50_000), "hello world");
    }

    #[test]
    fn cap_text_zero_means_no_cap() {
        let big = "x".repeat(10_000);
        assert_eq!(cap_text(&big, 0), big);
    }

    #[test]
    fn cap_text_truncates_with_marker_on_char_boundary() {
        // 10 multibyte chars (é = 2 bytes each); cap at 4 chars.
        let s = "é".repeat(10);
        let out = cap_text(&s, 4);
        assert!(out.starts_with(&"é".repeat(4)));
        assert!(out.contains("[truncated 6 chars]"));
        // Never split a codepoint: the kept prefix is valid UTF-8 of 4 'é's.
        assert_eq!(out.chars().take_while(|&c| c == 'é').count(), 4);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-research-gateway cap_text`
Expected: compile FAIL — `cap_text` not defined.

- [ ] **Step 3: Implement `cap_text`**

Add near `html_to_text` in `fetcher.rs`:

```rust
/// Cap `text` to at most `max_chars` characters for the tool result the worker
/// feeds into its LLM context — a full page can otherwise overflow the model
/// (deep-research turns died with anthropic 400 "prompt is too long"). Counts
/// CHARACTERS (not bytes) and cuts on a codepoint boundary. `max_chars == 0`
/// disables the cap (operator opt-out). On truncation, appends a marker naming
/// how many chars were dropped so the model knows the text was cut.
pub(crate) fn cap_text(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return text.to_string();
    }
    // char_indices()nth gives the byte offset of the (max_chars)-th char, i.e.
    // a guaranteed codepoint boundary; None means the text is already shorter.
    match text.char_indices().nth(max_chars) {
        None => text.to_string(),
        Some((byte_idx, _)) => {
            let dropped = text.chars().count() - max_chars;
            format!("{}\n…[truncated {dropped} chars]", &text[..byte_idx])
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-research-gateway cap_text`
Expected: 3 tests PASS.

Run: `cargo clippy -p mur-research-gateway -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/fetcher.rs
git commit -m "feat(gateway): cap_text helper for bounding fetched page text"
```

---

### Task 2: `max_fetch_chars` config knob

**Files:**
- Modify: `mur-research-gateway/src/config.rs` (const + env name + `GatewayConfig` field + `GatewayConfigYaml` field + resolve in `load_from_yaml` + a test)

**Interfaces:**
- Consumes: nothing from Task 1 (config is independent).
- Produces: `GatewayConfig.max_fetch_chars: usize`. Task 3 reads `self.config.max_fetch_chars`.

- [ ] **Step 1: Write the failing test** (append to `config.rs` tests; follow the existing env-serialized test idiom that uses the module's `Mutex` lock — mirror how a `search_limit` / `timeout_secs` test is written in that file)

```rust
    #[test]
    fn max_fetch_chars_default_env_yaml_precedence() {
        let _g = ENV_LOCK.lock().unwrap();
        // Default when nothing set.
        unsafe { std::env::remove_var("MUR_RESEARCH_MAX_FETCH_CHARS"); }
        let cfg = load_from_yaml("", std::path::Path::new("/tmp"));
        assert_eq!(cfg.max_fetch_chars, DEFAULT_MAX_FETCH_CHARS);
        // YAML sets it.
        let cfg = load_from_yaml(
            "research_gateway:\n  max_fetch_chars: 1234\n",
            std::path::Path::new("/tmp"),
        );
        assert_eq!(cfg.max_fetch_chars, 1234);
        // Env overrides YAML.
        unsafe { std::env::set_var("MUR_RESEARCH_MAX_FETCH_CHARS", "42"); }
        let cfg = load_from_yaml(
            "research_gateway:\n  max_fetch_chars: 1234\n",
            std::path::Path::new("/tmp"),
        );
        assert_eq!(cfg.max_fetch_chars, 42);
        unsafe { std::env::remove_var("MUR_RESEARCH_MAX_FETCH_CHARS"); }
    }
```

Adaptation note: the lock is named `ENV_LOCK` in the plan; use whatever the existing config tests name their `static Mutex` (grep the tests module — the "std::env::set_var … serializes on this lock" comment marks it). Match the existing tests' exact YAML key nesting (`research_gateway:` block) — confirm by reading a sibling test like one that sets `search_limit`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-research-gateway max_fetch_chars`
Expected: compile FAIL — `DEFAULT_MAX_FETCH_CHARS` / `max_fetch_chars` not defined.

- [ ] **Step 3: Add the const, env name, and fields**

In `config.rs`, next to `DEFAULT_SEARCH_LIMIT` (line ~39):

```rust
/// Default cap on the CHARACTERS of `fetch` page text returned to the worker.
/// A full page can otherwise overflow the model's context (deep-research turns
/// died with anthropic 400 "prompt is too long"). ~12–15k tokens/fetch; ~10
/// fetches fit a 200k window with reasoning room. `0` disables the cap.
pub const DEFAULT_MAX_FETCH_CHARS: usize = 50_000;
```

Next to `const ENV_SEARCH_LIMIT` (line ~73):

```rust
const ENV_MAX_FETCH_CHARS: &str = "MUR_RESEARCH_MAX_FETCH_CHARS";
```

Add to `GatewayConfig` (after `search_limit`, line ~88):

```rust
    /// Max characters of `fetch` page text returned to the worker; `0` = no cap.
    pub max_fetch_chars: usize,
```

Add to `GatewayConfigYaml` (after `search_limit`, line ~104):

```rust
    max_fetch_chars: Option<usize>,
```

- [ ] **Step 4: Resolve it in `load_from_yaml`**

After the `search_limit` resolution block (line ~152-155), add (note: NO clamp — 0 is the valid "no cap" sentinel):

```rust
    let max_fetch_chars = env_usize(ENV_MAX_FETCH_CHARS)
        .or(raw.max_fetch_chars)
        .unwrap_or(DEFAULT_MAX_FETCH_CHARS);
```

And add `max_fetch_chars` to the `GatewayConfig { … }` constructor (after `search_limit,`):

```rust
        search_limit,
        max_fetch_chars,
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p mur-research-gateway max_fetch_chars && cargo test -p mur-research-gateway config`
Expected: new test + all pre-existing config tests PASS.

Run: `cargo clippy -p mur-research-gateway -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-research-gateway/src/config.rs
git commit -m "feat(gateway): max_fetch_chars config knob (env + yaml, 0 = no cap)"
```

---

### Task 3: Apply the cap in `handle_fetch`

**Files:**
- Modify: `mur-research-gateway/src/server.rs` (`handle_fetch` — cap `result.text` in the fetch success arms; add a test)

**Interfaces:**
- Consumes: `fetcher::cap_text` (Task 1), `self.config.max_fetch_chars` (Task 2).
- Produces: no external API change — the `fetch` result shape is identical; only `text` is bounded.

- [ ] **Step 1: Write the failing test** (append to `server.rs` tests; a pure helper test — do NOT drive a real fetch)

Add a small seam so the cap is testable without network. In `handle_fetch`, the cap is applied via `fetcher::cap_text(&result.text, self.config.max_fetch_chars)`. Test `cap_text` composition directly here to lock the wiring intent:

```rust
    #[test]
    fn fetch_text_is_capped_to_config_budget() {
        // The server caps fetched text via fetcher::cap_text with the config
        // budget; verify the budget is actually applied (regression guard for
        // the handle_fetch wiring).
        let text = "a".repeat(1000);
        let capped = fetcher::cap_text(&text, 100);
        assert!(capped.len() < text.len());
        assert!(capped.contains("[truncated"));
        // 0 budget = untouched.
        assert_eq!(fetcher::cap_text(&text, 0), text);
    }
```

(If `cap_text` is `pub(crate)`, this test in the same crate can call `fetcher::cap_text`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-research-gateway fetch_text_is_capped`
Expected: FAIL if `cap_text` isn't reachable / not yet wired — it should compile once Task 1 landed; this test mainly guards Step 3's wiring. If it already passes on `cap_text` alone, proceed to Step 3 and rely on the manual smoke to confirm the handle_fetch wiring.

- [ ] **Step 3: Wire the cap into `handle_fetch`**

In `server.rs` `handle_fetch`, cap `result.text` at every fetch success point BEFORE serializing. There are two success arms: the rendered path (the unified `match result { Ok(result) => … }` after the escalation block) and the tier-1 path (`match fetcher::fetch_tier1(...) { Ok(result) => … }`). In each `Ok(result)` arm, insert one line before building the response:

Rendered arm:
```rust
                Ok(result) => {
                    audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                    let mut result = result;
                    result.text = fetcher::cap_text(&result.text, self.config.max_fetch_chars);
                    Response::success(
                        id,
                        serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                    )
                }
```

Tier-1 arm (find `match fetcher::fetch_tier1(&url, deny, self.config.timeout).await { Ok(result) => …`), apply the same two inserted lines (`let mut result = result;` + `result.text = fetcher::cap_text(...)`) before `Response::success`.

(Leave the `Err` arms, audit calls, and tier labels unchanged. `result.text` is a `String` field of `fetcher::FetchResult`, so it is reassignable.)

- [ ] **Step 4: Run tests + a live smoke**

Run: `cargo test -p mur-research-gateway && cargo clippy -p mur-research-gateway --all-targets -- -D warnings && cargo fmt --check`
Expected: all PASS/clean.

Live smoke (a large real page, unsandboxed — proves the cap end-to-end):
```bash
cargo build --release -p mur-research-gateway
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"fetch","arguments":{"url":"https://en.wikipedia.org/wiki/Rust_(programming_language)","render":false}}}' \
  | MUR_RESEARCH_MAX_FETCH_CHARS=2000 ./target/release/mur-research-gateway \
  | python3 -c "import sys,json; d=[json.loads(l) for l in sys][-1]; t=d['result']['text']; print('len',len(t)); print('has marker', '[truncated' in t)"
```
Expected: `len` ≈ 2000-ish and `has marker True` (the Wikipedia page is far larger than 2000 chars).

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/server.rs
git commit -m "fix(gateway): cap fetch text to max_fetch_chars (bound worker context) (deep-research)"
```

---

### Task 4: Documentation

**Files:**
- Modify: `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md` (tiers/config section — note the fetch content budget)

**Interfaces:** none (docs only).

- [ ] **Step 1: Add the note** (verbatim, inside the existing gateway/config section — do not create a new section)

```markdown
**Fetch content budget.** `fetch` caps the page text it returns to the worker
at `max_fetch_chars` (default 50 000 chars; env `MUR_RESEARCH_MAX_FETCH_CHARS`,
YAML `research_gateway.max_fetch_chars`; `0` disables). Without this a single
large page overflowed the worker's LLM context (`anthropic 400: "prompt is too
long"`), failing the turn before it could reply. The 5 MB body cap
(`MAX_BODY_BYTES`) bounds transfer/memory; `max_fetch_chars` bounds context.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md
git commit -m "docs(gateway): document max_fetch_chars fetch content budget"
```

---

## Operator Verification (manual, after merge — the convergence payoff)

1. Rebuild + reinstall the gateway: `cargo build --release -p mur-research-gateway`; copy to the workers' gateway path (`/opt/homebrew/bin/mur-research-gateway`) or re-point via `mur agent mcp add … --command <release path> --force` and `mur agent mcp set-network dr_worker_N research-gateway --broad-audited --yes` (the mcp re-add drops the egress grant — re-grant it).
2. Restart `dr_worker_1..4`; run one live iteration: `mur deep-research run deep-research --max-iterations 1`.
3. **Expected (fixed):** worker stderr has NO `prompt is too long`; the worker turn reaches `Completed`; a signed `Agent{dr_worker_N}` reply event lands in `~/.mur/channels/fleet-deep-research/events.jsonl`; `Step s1` succeeds (no `exit 1`).
4. Full run: `mur deep-research run deep-research` (budgeted, multi-iteration) — with turns completing and workers writing replies, the router can drive toward the `RESEARCH_COMPLETE` marker. Capture the synthesized report + real spend. This is the first end-to-end native deep-research report.
5. If a turn still overflows, lower `MUR_RESEARCH_MAX_FETCH_CHARS` (e.g. 20 000) or bound the worker's per-turn fetch count in the `deep-research-worker` skill — the mechanical cap is in place; the rest is tuning.

## Out of Scope (tracked separately)

- **Runtime-side tool-result cap:** a general `ToolResultEntry.content` truncation in `task_runner.rs` (protecting *every* tool, not just the gateway) — broader behavior change; do it only if non-gateway tools are observed to overflow context.
- **Main-content extraction** (readability-style): returning only a page's article body instead of the first N chars — a quality improvement, not needed to unblock convergence.
- **Per-turn fetch-count discipline** in the worker skill — behavioral tuning, revisit if the char cap alone doesn't fit realistic research turns.

## Self-Review Notes

- Spec coverage: overflow → Task 1 (`cap_text`) + Task 3 (wire into fetch); config knob → Task 2; docs → Task 4; convergence proof → operator verification.
- Type consistency: `cap_text(text: &str, max_chars: usize) -> String` produced in Task 1, consumed in Task 3; `GatewayConfig.max_fetch_chars: usize` produced in Task 2, read in Task 3. `0` sentinel handled uniformly in `cap_text` and unclamped in config.
- No hardcoded values: `DEFAULT_MAX_FETCH_CHARS` const + env + yaml, mirroring `search_limit`.
- Search path is deliberately untouched (snippets are already short).
