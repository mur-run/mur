# G2: Browserless HTTP Search Tier — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the research-gateway `search` tool work under the worker kernel sandbox by routing it through the same tier-1 reqwest path `fetch` already uses (which G1 proved works via the egress proxy) — instead of spawning `agent-browser`, which the sandbox denies. This unblocks worker research turns from completing, which is the true critical-path blocker for the deep-research fleet.

**Architecture:** The `search` tool already targets DuckDuckGo's server-rendered HTML endpoint (`https://html.duckduckgo.com/html/?q=…`) — a plain HTML page that needs no browser. Today it drives `agent-browser` to render+snapshot that page; under the sandbox `agent-browser` can't spawn (`Operation not permitted, os error 1`). Fix: fetch the endpoint with the tier-1 reqwest client (proxy-honoring, sandbox-friendly) plus a browser-like User-Agent, and parse DuckDuckGo's result anchors directly. No sandbox, runtime, or dependency change.

**Tech Stack:** Rust (edition 2024), `mur-research-gateway` only. Existing deps only (`reqwest`, `url` — no HTML-parser crate added; a focused string scan handles DDG's stable result markup).

## Root Cause (empirically pinned, 2026-07-09 live fleet run)

Worker telemetry showed `mcp__research-gateway__search` failing every call
(`ok=false`, 81×) while `fetch` succeeded (`ok=true`, 10×) after G1. Direct
reproduction under a sandboxed worker:
`search failed: spawn agent-browser: Operation not permitted (os error 1)`.

Downstream impact (why this is critical-path, not cosmetic): a worker's
research turn loops on failing searches, burns its agentic iteration budget
(~$1.42/turn observed), and ends in `TaskOutcome::Failed` — never
`Completed`. The `channel/delegate` self-append fires only on `Completed`
(by design — never sign user input as a reply), so the worker emits no
channel reply; the DAG executor sees an empty reply
(`dag.rs` `extract_agent_reply` → `exit_code = 1`), fails the step, and the
fleet aborts before any convergence marker. Fixing search lets turns
complete, which unblocks G3's live channel-write and fleet convergence.

Why HTTP suffices (verified live 2026-07-09): a plain GET of
`https://html.duckduckgo.com/html/?q=rust` returns server-rendered result
anchors — `class="result__a" href="//duckduckgo.com/l/?uddg=<percent-encoded
real URL>&rut=…"`. **A browser-like `User-Agent` is required** — without one
DDG returns HTTP 202 (a challenge interstitial, no results); with
`Mozilla/5.0…` it returns 200 + parseable anchors. `fetch_tier1` sets no
UA today, so the search path must add one.

## Global Constraints

- **No new dependency:** parse DDG's HTML with a focused string scan (the codebase's existing `html_to_text` is manual too) + the `url` crate for percent-decoding the `uddg` param. Do NOT add `scraper`/`select`/`regex`.
- **Reuse the SSRF guard:** the search fetch must go through `screen_url_blocking` exactly like `fetch_tier1` (screens `html.duckduckgo.com`); returned hit URLs are inert data — the worker's later `fetch` on them re-screens.
- **Proxy-honoring client:** the search reqwest client must be built the same way as `fetch_tier1`'s (default env-proxy honoring, so `HTTPS_PROXY` from the runtime reaches it), plus the required User-Agent. `redirect: none` is fine (DDG returns 200 directly).
- **Body cap preserved:** reuse the same `MAX_BODY_BYTES` streaming cap as `fetch_tier1` (don't `resp.text()` an unbounded body).
- **SearchHit shape unchanged:** `{ title, url, snippet }` (snippet best-effort, may be empty — already tolerated).
- No hardcoded values (User-Agent + endpoint as named consts); single source file ≤ 800 lines; `cargo clippy --workspace -- -D warnings` + `cargo fmt --check` clean.
- Tests: `export ORT_STRATEGY=download`; `cargo test -p mur-research-gateway`. Parser tests are hermetic (embedded HTML fixture, NO live network).

---

### Task 1: Tier-1 search in `fetcher.rs` (client UA + DDG parser)

**Files:**
- Modify: `mur-research-gateway/src/fetcher.rs` — move `SearchHit` here (from `browser.rs`), add `SEARCH_USER_AGENT` + `DDG_HTML_ENDPOINT` consts, a shared `build_client`, `search_tier1`, and `parse_ddg_hits`.

**Interfaces:**
- Produces: `pub struct SearchHit { pub title: String, pub url: String, pub snippet: String }` (moved here); `pub async fn search_tier1(query: &str, limit: usize, deny: &[String], timeout: Duration) -> Result<Vec<SearchHit>, FetchError>`. Task 2 consumes `search_tier1`; `browser.rs` stops defining `SearchHit`.

- [ ] **Step 1: Write the failing parser test** (hermetic fixture; append to `fetcher.rs` tests)

```rust
    #[test]
    fn parses_ddg_result_anchors_and_decodes_uddg() {
        // Trimmed real DDG html endpoint shape (2026-07-09): result links are
        // `result__a` anchors whose href wraps the real URL in the `uddg`
        // query param; snippet is a following `result__snippet` element.
        let html = r#"
        <div class="result">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&amp;rut=x">First <b>Title</b></a>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa">Snippet one text.</a>
        </div>
        <div class="result">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=y">Second</a>
        </div>"#;
        let hits = parse_ddg_hits(html, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://example.com/a");
        assert_eq!(hits[0].title, "First Title"); // tags stripped
        assert_eq!(hits[0].snippet, "Snippet one text.");
        assert_eq!(hits[1].url, "https://rust-lang.org/");
        assert_eq!(hits[1].snippet, ""); // no snippet element → empty, tolerated
    }

    #[test]
    fn parse_ddg_hits_respects_limit() {
        let block = |u: &str| format!(
            r#"<a class="result__a" href="//duckduckgo.com/l/?uddg={u}">t</a>"#,
            u = u
        );
        let html = format!("{}{}{}", block("https%3A%2F%2Fa.test%2F"), block("https%3A%2F%2Fb.test%2F"), block("https%3A%2F%2Fc.test%2F"));
        assert_eq!(parse_ddg_hits(&html, 2).len(), 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-research-gateway parse_ddg_hits parses_ddg`
Expected: compile FAIL — `parse_ddg_hits` / `SearchHit` not defined in `fetcher`.

- [ ] **Step 3: Move `SearchHit` + add consts and parser**

Move the `SearchHit` struct definition from `browser.rs` to `fetcher.rs` (keep its derives — it's `Serialize`; check the exact derives on the original at `browser.rs:32` and preserve them). Add near the top of `fetcher.rs`:

```rust
/// Browser-like UA — DuckDuckGo's html endpoint returns HTTP 202 (a challenge
/// interstitial, no results) to requests without one. Verified live 2026-07-09.
const SEARCH_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// DuckDuckGo's server-rendered (no-JS) HTML search endpoint.
const DDG_HTML_ENDPOINT: &str = "https://html.duckduckgo.com/html/";
```

Add the parser (focused scan; `url` crate decodes the `uddg` param):

```rust
/// Parse DuckDuckGo html-endpoint results into hits. Keys on `result__a`
/// anchors whose href wraps the real URL in the `uddg` query param; the
/// snippet is the nearest following `result__snippet` text. Deliberately
/// minimal — DDG's markup is not a contract MUR controls (spec §11), so a
/// focused scan beats pulling in an HTML-parser dependency.
fn parse_ddg_hits(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    // Split on the result-anchor class marker; each piece after the first
    // starts inside a result__a tag.
    let mut segments = html.split("class=\"result__a\"");
    let _ = segments.next(); // preamble before the first anchor
    for seg in segments {
        if hits.len() >= limit {
            break;
        }
        let Some(href) = attr_after(seg, "href=\"") else {
            continue;
        };
        let Some(url) = decode_uddg(&href) else {
            continue;
        };
        let title = strip_tags(inner_text_after_tag(seg));
        // Snippet: nearest following result__snippet inner text, if any before
        // the next result anchor (segments already end at the next anchor).
        let snippet = seg
            .split_once("class=\"result__snippet\"")
            .and_then(|(_, rest)| Some(strip_tags(inner_text_after_tag(rest))))
            .unwrap_or_default();
        hits.push(SearchHit { title, url, snippet });
    }
    hits
}

/// Value of the first `needle`-prefixed attribute in `s` (up to the next `"`).
fn attr_after(s: &str, needle: &str) -> Option<String> {
    let start = s.find(needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Text between the first `>` and the next `<` after it (an element's inner
/// text run), HTML-entity-naive (DDG titles are plain).
fn inner_text_after_tag(s: &str) -> String {
    let Some(gt) = s.find('>') else {
        return String::new();
    };
    let rest = &s[gt + 1..];
    let end = rest.find('<').unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Strip any remaining inline tags (e.g. `<b>` in a title) and collapse
/// whitespace. Reuses the same tag-stripping idea as `html_to_text`.
fn strip_tags(s: String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decode DDG's redirect href `//duckduckgo.com/l/?uddg=<percent-encoded real
/// url>&rut=…` into the real URL. Also accepts a bare absolute URL (defensive).
fn decode_uddg(href: &str) -> Option<String> {
    let abs = if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{stripped}")
    } else {
        href.to_string()
    };
    let parsed = url::Url::parse(&abs).ok()?;
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
        return Some(v.into_owned());
    }
    // No uddg param → treat an http(s) href as already-real, else skip.
    (abs.starts_with("http://") || abs.starts_with("https://")).then_some(abs)
}
```

Add `search_tier1` + a shared `build_client` (refactor `fetch_tier1` to use it too, so the UA/timeout/redirect policy has one source):

```rust
/// Shared tier-1 reqwest client: env-proxy honoring (so the runtime's
/// `HTTPS_PROXY` reaches it — the G1 path), per-request timeout, a
/// browser-like UA (some hosts, incl. DDG's html endpoint, 202/deny without
/// one), and no auto-redirect (each hop is re-screened by the caller).
fn build_client(timeout: Duration) -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(SEARCH_USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))
}

/// Tier-1 web search: GET DuckDuckGo's html endpoint through the same
/// proxy-honoring reqwest path `fetch_tier1` uses (works under the kernel
/// sandbox; agent-browser does not — G2), then parse result anchors. Screens
/// the endpoint host via the SSRF guard exactly like a fetch.
pub async fn search_tier1(
    query: &str,
    limit: usize,
    deny: &[String],
    timeout: Duration,
) -> Result<Vec<SearchHit>, FetchError> {
    let mut search_url = url::Url::parse(DDG_HTML_ENDPOINT).expect("static URL is valid");
    search_url.query_pairs_mut().append_pair("q", query);

    let screened = screen_url_blocking(search_url.as_str(), deny)
        .await
        .map_err(FetchError::Guard)?;

    let client = build_client(timeout)?;
    let mut resp = client
        .get(screened.clone())
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;
    if let Some(len) = resp.content_length()
        && len > MAX_BODY_BYTES as u64
    {
        return Err(FetchError::TooLarge);
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?
    {
        if would_exceed(buf.len(), chunk.len(), MAX_BODY_BYTES) {
            return Err(FetchError::TooLarge);
        }
        buf.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&buf);
    Ok(parse_ddg_hits(&body, limit))
}
```

Then refactor `fetch_tier1` to build its client via `build_client(timeout)` (replaces its inline `reqwest::Client::builder()…build()` block) so the UA/redirect policy is single-sourced. This also gives `fetch` a real UA (strictly more robust; no behavior regression — the screen/cap/redirect logic is unchanged).

- [ ] **Step 4: Run tests**

Run: `cargo test -p mur-research-gateway`
Expected: new parser tests PASS; pre-existing fetcher tests (`refuses_private_target`, `refuses_denied_host`, `body_cap_*`) still PASS (they exercise `fetch_tier1`, whose logic is unchanged bar the shared client builder).

Run: `cargo clippy -p mur-research-gateway -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add mur-research-gateway/src/fetcher.rs
git commit -m "feat(gateway): browserless tier-1 web search (DDG html endpoint + UA) (G2)"
```

---

### Task 2: Route `search` to the tier-1 path; retire the browser search

**Files:**
- Modify: `mur-research-gateway/src/server.rs` — `handle_search` calls `fetcher::search_tier1` instead of `browser::search`.
- Modify: `mur-research-gateway/src/browser.rs` — remove the now-dead `search`, `parse_search_hits`, `parse_markdown_link`, and the moved `SearchHit` (keep `fetch_rendered`, `build_fetch_argv`, `run_agent_browser`, `preflight` — still used by `fetch` render:true tiers 2/3).

**Interfaces:**
- Consumes: `fetcher::search_tier1`, `fetcher::SearchHit` (Task 1).
- Produces: no external change — the `search` MCP tool's response shape is identical.

- [ ] **Step 1: Reroute `handle_search`**

In `server.rs` `handle_search`, replace:

```rust
        let cfg = &self.config.browser;
        let timeout = self.config.browser_timeout;
        match browser::search(&query, limit, deny, cfg, timeout).await {
```

with (search is tier-1 now — use the tier-1 fetch timeout, drop the browser cfg):

```rust
        let timeout = self.config.timeout;
        match fetcher::search_tier1(&query, limit, deny, timeout).await {
```

(Leave the `Ok(hits)` / `Err(e)` arms, the audit calls, and `fetch_outcome` mapping unchanged — `search_tier1` returns the same `Result<Vec<SearchHit>, FetchError>`.)

- [ ] **Step 2: Remove the dead browser search code**

In `browser.rs`, delete `pub async fn search(...)`, `fn parse_search_hits(...)`, `fn parse_markdown_link(...)`, the `SearchHit` struct (now in `fetcher.rs`), and any test exercising the markdown search parser. Update any `use` of `SearchHit` in `browser.rs`/`server.rs` to `fetcher::SearchHit`. Keep everything the `fetch` render path needs.

- [ ] **Step 3: Run the full crate suite + a live smoke**

Run: `cargo test -p mur-research-gateway && cargo clippy -p mur-research-gateway -- -D warnings && cargo fmt --check`
Expected: all PASS/clean; `declares_search_and_fetch` still passes (tool list unchanged).

Live smoke (network — outside sandbox, proves the endpoint+parser against real DDG):

```bash
cargo build --release -p mur-research-gateway
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"search","arguments":{"query":"rust async runtime","limit":3}}}' \
  | ./target/release/mur-research-gateway
```

Expected: `id:2` result with a non-empty `[{title,url,snippet}]` array of real URLs. If it returns empty, DDG may have changed markup or rate-limited — re-run once; if persistently empty, capture the raw body (temporarily log it) and adjust `parse_ddg_hits` before proceeding.

- [ ] **Step 4: Commit**

```bash
git add mur-research-gateway/src/server.rs mur-research-gateway/src/browser.rs
git commit -m "refactor(gateway): search uses the tier-1 HTTP path; retire agent-browser search (G2)"
```

---

### Task 3: Documentation

**Files:**
- Modify: `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md` — in the tiers/search section, note that `search` is tier-1 HTTP (DDG html endpoint via the egress proxy), not a browser tier, so it works under the worker sandbox; browser tiers remain only for `fetch` render:true.
- Modify: `docs/architecture/runtime-overview.md` if it describes the gateway tiers (grep "research-gateway" / "agent-browser"); otherwise skip with a note in the commit.

- [ ] **Step 1: Make the spec edit** (verbatim, inside the existing tiers section)

```markdown
**Search is tier-1 HTTP, not a browser tier.** `search` GETs DuckDuckGo's
server-rendered html endpoint through the same proxy-honoring reqwest path as
a tier-1 `fetch` (browser-like UA required; DDG 202s without one), so it works
under the worker kernel sandbox — unlike `agent-browser`, which the sandbox
denies (`Operation not permitted`). `agent-browser` remains only for `fetch`
with `render:true` (tiers 2/3).
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md docs/architecture/runtime-overview.md
git commit -m "docs(gateway): search is a browserless tier-1 HTTP path (G2)"
```

---

## Operator Verification (manual, after merge — the full deep-research payoff)

1. Rebuild + reinstall the gateway the workers spawn: `cargo build --release -p mur-research-gateway` (workers' `research-gateway` MCP command points at `/opt/homebrew/bin/mur-research-gateway` — reinstall via `build.sh` or copy the release binary there; re-pin if the sha guard trips: `mur agent mcp pin dr_worker_N research-gateway`). Also rebuild `mur-agent-runtime` if not current.
2. Start `dr_worker_1..4`; run one live iteration: `mur deep-research run deep-research --max-iterations 1`.
3. **Expected (fixed):**
   - Worker telemetry: `mcp__research-gateway__search ok=true` (was `false` every call).
   - Worker turns reach `TaskOutcome::Completed` → the delegate reply is non-empty → `Step s1` succeeds (no `exit 1`).
   - **G3 now exercised live:** a signed `Agent{dr_worker_N}` reply event appears in `~/.mur/channels/fleet-deep-research/events.jsonl`, `index/channels/channels.db` updates, and NO `readonly database` warning.
4. Full run: `mur deep-research run deep-research` (multi-iteration, budgeted) — with turns completing and workers writing replies, the router can drive toward the `RESEARCH_COMPLETE` marker. This is the first end-to-end native deep-research run; capture the synthesized report and the real spend.
5. If convergence still stalls with search working, the remaining suspect is G4 (fleet-scoped skills not injecting — the router/worker skills that shape decomposition/synthesis). Diagnose then per the G4 plan.

## Out of Scope (tracked separately)

- **G4** skill loader rejects `fleet:<name>` scoped refs — next after G2, if convergence needs the fleet skills.
- Search-provider robustness (DDG markup drift, a real search API) — spec §11 keeps a dedicated search API out of scope; the focused parser is intentionally minimal and easy to adjust.
- Pin-to-proxy DNS-rebinding closure (Phase 3 TODO already in `fetch_tier1`) — unchanged.

## Self-Review Notes

- The fix removes an entire failure mode (browser spawn under sandbox) rather than widening the sandbox — strictly less privilege, no new grant.
- Parser tests are hermetic (embedded fixture); the live smoke in Task 2 Step 3 is the only network touch and is a manual gate, not a CI test.
- `search_tier1` reuses `screen_url_blocking` + the `MAX_BODY_BYTES` streaming cap, so search inherits the same SSRF + DoS protections as `fetch`.
- `build_client` single-sources the UA/redirect/timeout for both fetch and search; `fetch` gaining a UA is a robustness improvement with no logic change.
