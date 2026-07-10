# MUR-Native Deep Research (Design)

Date: 2026-07-09
Status: Design approved; ready for implementation plan.

## 1. Goal

Give MUR a **native** deep-research capability — query decomposition → parallel
fan-out → adversarial verification → cited synthesis — that runs on MUR's own
orchestration primitives (**dynamic workflow + fleet + agents**), not on Claude
Code's built-in `deep-research` skill.

**Why native (the motivation that fixes the architecture):** the Claude Code
built-in already does this research well, but it runs as *host subagents* — the
work is Claude's, MUR only labels it (proven: yesterday's "AURA Mode 1" security
report was 100% Claude host subagents; the `aura` MUR agent lived 34 seconds and
touched nothing). Native buys three things the built-in structurally cannot:

1. **MUR orchestration owns it** — a fleet of MUR agents, driven by the router's
   dynamic per-iteration DAG (`cmd/fleet/plan.rs`), is the thing doing the research.
2. **Cryptographic provenance** — every claim is written into an Ed25519-signed
   channel by the worker that produced it (Unified Channel v3d per-actor signing).
   "AURA did this research" becomes provable, not a label.
3. **Platform integration** — budget accounting, kill-switch, Commander governance,
   scheduling, and long-term memory all ride the existing fleet/agent machinery.

**Non-goal:** out-parallelizing the built-in. Real concurrency is bounded either
way (~min(16, cores-2)). Native wins on ownership/provenance/governance, not speed.

## 2. What this replaces and reuses

Grounding (2026-07-09) established the exact reuse surface:

- **Reuse — fleet dynamic loop.** `mur fleet run --loop` with the router emitting a
  DAG each iteration (`cmd/fleet/plan.rs`), convergence via `done_when: marker:<TEXT>`,
  guards (iteration/deadline/stuck), real per-token budget, kill-switch, Commander
  hooks (`cmd/fleet/loop_run.rs`). This IS the "dynamic workflow" the user wants.
- **Reuse — per-server egress governance (#661, on main).** `McpNetMode::BroadAudited`
  = allow-all-except-`deny_hosts`, routed through the loopback CONNECT egress proxy
  (`sandbox/egress_proxy.rs`) with per-CONNECT audit; granted via
  `mur agent mcp set-network --broad-audited` (consent + `EgressAuthorization` +
  telemetry). See `2026-07-08-agent-egress-governance-design.md`.
- **Reuse — agent-browser + Lightpanda** (verified 0.31.1, 2026-07-08; see
  `gotcha_agent_browser_lightpanda_engine_dead`). Lightpanda binary already installed
  at a configurable path.
- **New — exactly two units:** a `research-gateway` MCP server (§5) and the
  research prompts/skills (§6). Everything else is composition.

**Rejected alternatives:**
- *Static DAG workflow + delegate* — the DAG is authored ahead of time; dynamic
  fan-out (sub-question count unknown until decomposition) doesn't fit.
- *Single agent + `parallel_jobs`* — its fan-out target is a CLI subprocess
  (claude/codex); either it has no reasoning (agent-browser = fetch, no research) or
  it spawns claude (back to non-native). The LLM worker must be a MUR fleet agent.

## 3. Architecture

```
┌─────────────────────────── fleet "deep-research" ───────────────────────────┐
│  router (mur)                                                                │
│   └─ per-iteration DAG (plan.rs):  decompose → research×N → verify → synth   │
│                                                                              │
│  worker_1..worker_k   (k = 8–16, configurable; ALL entitlements: restricted) │
│   each mounts ONE broad-audited MCP server:  research-gateway                 │
└──────────────────────────────────────────────────────────────────────────────┘
        │ MCP: search(query) / fetch(url)   ← read-only verbs, no free navigation
        ▼
┌──────────── research-gateway (Rust, MUR-shipped, broad-audited egress) ───────┐
│  preflight: agent-browser >= 0.28.0? lightpanda present?  (missing → degrade) │
│  SSRF guard: refuse private/link-local/loopback IPs (post-DNS recheck)         │
│  tier 1: reqwest GET                              (most pages, cheapest)       │
│  tier 2: agent-browser --engine lightpanda --args "" --session <per-fetch>    │
│  tier 3: agent-browser --engine chrome (+stealth)  (anti-bot / screenshot)    │
│  URL-level audit per call → channel/telemetry                                 │
└───────────────────────────────────────────────────────────────────────────────┘
```

Workers are the injectable surface and hold **no egress**. Only the deterministic
(no-LLM) gateway reaches the web. A prompt-injected worker can at most ask the
gateway to fetch a URL — logged, SSRF-guarded, and incapable of POSTing arbitrary
data to arbitrary hosts (the API is `fetch`, not "open a socket").

## 4. Dynamic flow (fleet run --loop)

The router emits a fresh DAG each iteration; the loop runs until the synthesis
marker appears. Sub-question count is decided dynamically at decompose time.

1. **Decompose** — router splits the question into sub-questions (may be 100+),
   writes them to the channel as a work queue.
2. **Research (×N iterations)** — router assigns a batch of sub-questions to workers
   (bounded by `max_concurrency`); each worker `search()`s, `fetch()`es top sources,
   extracts **claims each bound to a URL + supporting quote**, writes them to the
   channel. Repeats until the queue drains.
3. **Verify (3-vote adversarial)** — each claim is dispatched to 3 workers, each with
   a distinct refutation lens (correctness / source-independence / recency). A claim
   survives on a 2-of-3 confirm; else it is dropped (fail-safe = drop).
4. **Synthesize** — router folds confirmed claims into a cited report and emits
   `done_when: marker:RESEARCH_COMPLETE` on its own line → deterministic convergence.

Inherited for free: `--budget-usd` (real token accounting), kill-switch
(`mur fleet stop`), Commander kill/budget hooks (fail-closed), signed-channel
provenance per claim.

## 5. Component: `research-gateway` MCP server

A new, small, deterministic Rust MCP server shipped with MUR (BusyBox-style or a
dedicated binary; decided in the plan). **No LLM.** This is the single trust boundary
that holds egress.

**Tools exposed to workers (read-only):**
- `search(query, limit?) -> [{title, url, snippet}]`
- `fetch(url, render?) -> {url, status, title, text, fetched_at}`

**Internals:**
- **Preflight** — verify `agent-browser >= 0.28.0` and the Lightpanda binary exist;
  if missing, degrade to tier 1 and surface an explicit warning (never silently).
- **Escalation ladder (deterministic code, not a skill):**
  - tier 1 `reqwest` GET — default; most pages.
  - tier 2 `agent-browser --engine lightpanda --executable-path <cfg> --args "" --session <per-fetch>`
    — JS pages. `--args ""` is mandatory (Chrome stealth args break Lightpanda).
  - tier 3 `agent-browser --engine chrome --args "<stealth,comma-separated>"` — anti-bot /
    screenshot. Chrome launch flags go through a SINGLE `--args` value, never bare argv:
    a bare `--no-sandbox` is parsed by agent-browser as a subcommand and errors with
    "Unknown command" (so the chrome tier silently never launched until fixed 2026-07-09).
  - **lightpanda → chrome escalation** (rendered `fetch` only): a tier-2 attempt escalates
    to tier 3 when lightpanda "doesn't work" — an `Http` failure (spawn/timeout/non-zero
    exit) OR a success that rendered **no text** (the engine ran but produced nothing, an
    exit-0-empty a plain error check misses). `Guard`/`TooLarge` are tier-independent and
    never escalate. `chrome:true` forces tier 3 directly.
  - **Search is tier-1 HTTP, not a browser tier.** `search` GETs DuckDuckGo's server-rendered
    html endpoint through the same proxy-honoring reqwest path as a tier-1 `fetch` (a
    browser-like User-Agent is required — DDG returns HTTP 202 without one), so search works
    under the worker kernel sandbox — `agent-browser` cannot spawn there (`Operation not
    permitted`). `agent-browser` is used only for `fetch` with `render:true`. No API key for v1.
- **Fetch content budget.** `fetch` caps the page text it returns to the worker at
  `max_fetch_chars` (default 50 000 chars; env `MUR_RESEARCH_MAX_FETCH_CHARS`, YAML
  `research_gateway.max_fetch_chars`; `0` disables). Without this a single large page overflowed
  the worker's LLM context (`anthropic 400: "prompt is too long"`), failing the turn before it
  could reply. The 5 MB body cap (`MAX_BODY_BYTES`) bounds transfer/memory; `max_fetch_chars`
  bounds context. `search` results (short title/url/snippet) are not capped.
- **Per-fetch session isolation** — each fetch uses a unique `--session` id (verified
  isolated, 2026-07-08) so concurrent worker fetches never cross-contaminate state.
- **Daemon lifecycle** — manage agent-browser's persistent daemon; detect version
  mismatch; `pkill -f agent-browser` on staleness.
- **SSRF guard (hard rule, non-configurable)** — refuse any URL whose resolved IP is
  private / link-local / loopback / unique-local, checked at screen time on every
  tier. Connect-time re-resolution (the DNS-rebinding window between screen and
  connect) is NOT closed yet — same advisory-enforcement framing as §7 item 4:
  deferred to Phase 3, not rebind-proof today.
- **Where `deny_hosts` + SSRF are enforced (tier-dependent — load-bearing).** The
  egress proxy only sees tier-1 (`reqwest`) connections; the tier-2/3 browser
  subprocesses open their own connections the proxy cannot observe. Therefore, for
  the browser tiers, `deny_hosts` and the SSRF guard are enforced **in gateway code
  before spawning the browser** (URL pre-filtered; refused URLs never reach
  agent-browser). Proxy-layer deny/audit is a backstop for tier 1 only; the gateway's
  own pre-spawn check + URL-level audit is the sole enforcement/evidence for tiers
  2/3. This is not a dependency on egress-governance Phase 3 — it is the gateway
  doing its own job; Phase 3 (sbpl pin-to-proxy) later makes it airtight.
- **URL-level audit** — every call logs `{worker, url, tier, outcome}` to the channel
  and telemetry, giving request-level auditability above the proxy's host-level
  CONNECT log.

**Config (no hardcoded values):** Lightpanda executable path, engine defaults,
worker count, per-request timeout, `deny_hosts` overlay — all from
`~/.mur/config.yaml` / fleet config / env, never literals.

## 6. Component: research prompts / skills

Reshape the five existing `aura-*` skills into the fleet roles (scope: **Fleet**, so
they inject only for `deep-research` members):

- **Router** — decompose prompt (breadth → sub-questions), assignment prompt (batch →
  workers), synthesis prompt (confirmed claims → cited report + marker).
- **Worker (research)** — `search`/`fetch` via gateway, extract claims with citations
  (reuse `aura-citation-discipline`, `aura-source-triangulation`).
- **Worker (verify)** — adversarial refutation under one assigned lens.

The escalation ladder that was `aura-research-escalation-ladder` (advisory, LLM-facing)
becomes **deterministic gateway code** (§5) — a strict upgrade; the skill is retired.

## 7. Egress governance alignment

This fleet is the **first real workload** for the #661 egress governance model, and
must honor it exactly (cross-checked against `2026-07-08-agent-egress-governance-design.md`). A critical ordering constraint was discovered in the 2026-07-09 live fleet run: the loopback egress proxy must start before the B1 kernel sandbox seals, with its listener port carved into the sandbox profile as a loopback-only rule, or sandboxed children cannot dial the proxy their environment points at.

1. **Grant via the shipped consent path only.** Provisioning runs
   `mur agent mcp set-network <agent> research-gateway --broad-audited` per worker —
   one explicit operator consent, recorded as `EgressAuthorization`. Fleet creation
   NEVER opens egress implicitly ("it's a research fleet" is not consent).
2. **Two-layer audit.** Proxy per-CONNECT (host-level) + gateway per-call (URL-level).
   Every report citation reconciles to one gateway audit record.
3. **SSRF guard** (§5) — broad-audited's "all" must exclude internal networks;
   non-negotiable, on top of `deny_hosts`. For the browser tiers, both `deny_hosts`
   and SSRF are enforced **in gateway code pre-spawn** (§5) — the proxy cannot see
   browser-subprocess connections, so it is not the enforcement point there.
4. **Advisory-enforcement honesty.** tier 1 (reqwest) honors the proxy; tier 2/3
   browser subprocesses may not — mitigated by universal gateway URL audit, and
   documented (not overclaimed) as "airtight = Phase 3 sbpl pin-to-proxy, which then
   pins only this one gateway."
5. **Export/import safety (free, shipped).** `.fleet` import downgrades broad-audited
   → `inherit` and clears authorization; a shared deep-research fleet has no egress
   until re-granted locally.
6. **Commander revocation point.** Reserve the single gateway entry as the revocation
   target for the future `revoke-egress` directive (Phase 2); no code now.

Net: one gateway choke point means Phase 2 (request-level audit) is partially realized
here today, and Phase 3 (DLP / pin-to-proxy) later touches exactly one point.

## 7a. Provisioning and Tool Rules

### Headless HITL: gateway tool pre-approval

`provision` stamps one `ToolRule` per worker:
`{ pattern: mcp__research-gateway__*, policy: allow }`.

Rationale: fleet-delegated turns are headless — the `tool/approval_needed`
prompt the runtime emits on the default `ask` policy has no answerer, so
risk-tiered tool calls dead-end in a 300 s timeout → deny → `state: failed`
(the operator-E2E blocker). The two gateway tools are read-only and fully
governed downstream (SSRF guard, deny-hosts, audit log), and the rule grants
no egress by itself: the gateway's outbound stays Inherit/restricted until
the separate explicit-consent `--grant-egress` step. The consent boundary is
unchanged; only the redundant per-call prompt (which nothing can answer) is
removed, and only for this one server's tools. Everything else keeps the
fail-closed `ask` default.

## 8. Component boundaries

| Unit | Responsibility | Depends on |
|------|----------------|------------|
| fleet `deep-research` (config) | roster (router + k workers), goal, `done_when`, budget | fleet machinery (existing) |
| router prompts | decompose / assign / synthesize | LLM; channel |
| worker prompts (research/verify) | search+fetch+extract; adversarial verify | `research-gateway` MCP |
| `research-gateway` MCP (new Rust) | search/fetch, escalation, SSRF, audit, daemon | agent-browser + Lightpanda subprocess |
| egress grant (config) | per-worker broad-audited on the gateway server | `mcp set-network` (#661) |

Each is independently testable (§9).

## 9. Testing

- **Gateway (unit)** — tier selection; `--args ""` on lightpanda; SSRF guard blocks
  private/link-local/loopback + post-DNS recheck; `deny_hosts` respected; audit line
  emitted per call; preflight degrades on missing binary.
- **Orchestration (integration)** — run the full loop against a **stub gateway**
  (fixed corpus, deterministic) to TDD decompose→fan-out→verify→synthesize and marker
  convergence, with no network.
- **E2E (one small real question)** — swap in the real broad-audited gateway; confirm
  a cited report with per-claim provenance in the signed channel; confirm every
  citation has a matching gateway audit record.

## 10. Delivery sequencing (de-risk)

1. `research-gateway` MCP (tier 1 + SSRF + audit + stub search) — unit-tested alone.
2. Fleet config + prompts; orchestration TDD against the stub gateway.
3. Wire agent-browser tiers 2/3 into the gateway.
4. Grant broad-audited per worker (consent path); E2E on one real question.
5. Optional later: shared single-instance gateway (remote MCP + fetch cache) if
   duplicate-fetch waste is measured; scheduling + long-term memory (the Mode-2
   persistence story) as a follow-on.

## 11. Out of scope (this spec)

- Search-provider API backends (Brave/SerpAPI) — v1 uses agent-browser-driven search.
- Shared single-process gateway / cross-worker fetch cache (measure first).
- Scheduling + long-term research memory (follow-on; the fleet runs one job at a time).
- DLP / rate-limiting / sbpl pin-to-proxy (egress governance Phase 3).
- Commander `revoke-egress` directive (egress governance Phase 2).
