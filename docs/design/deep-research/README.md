<!-- Languages: [English](README.md) · [繁體中文](README.zh-TW.md) · [简体中文](README.zh-CN.md) · [日本語](README.ja.md) · [한국어](README.ko.md) -->

# MUR Deep Research — Detailed Design

> Native deep research, owned end to end by a MUR fleet.
> Shipped in **v2.45.0** (2026-07-10), PRs #663–#672.

A sandboxed squad of MUR agents decomposes a question, researches the live web through a single audited gateway, adversarially verifies each claim, and converges on a **cited, cryptographically-attributed report** — on MUR's own orchestration, not host subagents.

- **Spec:** `docs/superpowers/specs/2026-07-09-mur-native-deep-research-design.md`
- **Crate:** `mur-research-gateway` · **Command:** `mur-core/src/cmd/deep_research`

---

## §1 · Why native, not host subagents

Claude Code's built-in deep-research already researches well — but it runs as *host subagents*. The work is Claude's; MUR only labels it. Native orchestration is what makes the result *MUR's*, and provable.

- **MUR orchestration owns it.** A fleet of MUR agents, driven by the router's dynamic per-iteration DAG (`cmd/fleet/plan.rs`), is the thing doing the research — not a subagent the host spawned and forgot.
- **Cryptographic provenance.** Every claim is written into an Ed25519-signed channel by the worker that produced it (Unified Channel v3d-2, *peer-writes-own*). "This agent found this" becomes verifiable, not a caption.
- **Platform integration.** Real per-token budget, kill-switch, Commander governance, scheduling, and long-term memory all ride the existing fleet/agent machinery — for free.

**Non-goal:** out-parallelizing the built-in. Concurrency is bounded either way (~`min(16, cores−2)`). Native wins on ownership, provenance, and governance — not raw speed.

---

## §2 · Architecture — three layers, one choke point

Workers hold **no** egress and are the only injectable surface. Only the deterministic, no-LLM gateway reaches the web — and it does so from inside an enforced kernel sandbox.

```
┌─────────────────────── fleet "deep-research" ───────────────────────┐
│  router (mur)  — dynamic per-iteration DAG (plan.rs)                 │
│  done_when: marker:RESEARCH_COMPLETE · budget-usd · deadline · kill  │
└──────────────────────────────┬──────────────────────────────────────┘
             │ channel/delegate (Ed25519-signed reply)
             ▼
┌──────────────── worker × k  (entitlements: restricted) ─────────────┐
│  model_ref → real LLM · mounts 1 MCP: research-gateway              │
│  tools: search · fetch      built-ins denied                        │
└──────────────────────────────┬──────────────────────────────────────┘
             │ MCP: search(query) / fetch(url)   — read-only verbs
             ▼
┌────────── research-gateway (Rust, no LLM, broad-audited egress) ─────┐
│  SSRF guard · tier ladder 1→2→3 · content budget · per-call audit    │
└──────────────────────────────────────────────────────────────────────┘
       enforced under: B1 kernel sandbox + loopback egress proxy
```

A prompt-injected worker can at most ask the gateway to `fetch` a URL — logged, SSRF-guarded, and incapable of POSTing arbitrary data to arbitrary hosts. The API is `fetch`, not "open a socket."

---

## §3 · Dynamic flow — decompose → research → verify → synthesize

The router emits a fresh DAG each iteration of `mur fleet run --loop`; sub-question count is decided at decompose time. The loop runs until the synthesis marker appears on its own line.

1. **Decompose** — the router splits the question into sub-questions (may be 100+) and writes them to the channel as a work queue.
2. **Research (×N iterations)** — the router assigns a batch to workers (bounded by `max_concurrency`). Each worker `search()`es, `fetch()`es the top sources, and extracts **claims each bound to a URL + a supporting quote**, writing them back as a signed reply. Repeats until the queue drains.
3. **Verify (3-vote adversarial)** — each claim is dispatched to three workers, each with a distinct refutation lens: correctness / source-independence / recency. A claim survives on a 2-of-3 confirm; otherwise it is dropped (fail-safe = drop).
4. **Synthesize → converge** — the router folds confirmed claims into a cited report and emits `RESEARCH_COMPLETE` on its own line. The structured `done_when: marker:…` converges deterministically — no extra LLM call, and prose that merely quotes the marker can't false-converge.

**Inherited for free:** `--budget-usd` with *real* per-token accounting · `mur fleet stop` kill-switch · Commander kill/budget hooks (fail-closed) · signed-channel provenance per claim · iteration-cap / deadline / stuck-detection guards.

---

## §4 · The research-gateway

A small, dependency-light Rust MCP server shipped with MUR. Two read-only verbs; a fixed, code-driven tier ladder (no LLM decides tiers); every byte of egress governed.

```
search(query, limit?)  →  [{title, url, snippet}]
fetch(url, render?)    →  {url, status, title, text, tier}
```

### Escalation ladder (deterministic code, not a skill)

| Tier | Engine | Notes |
|------|--------|-------|
| **1 · http** | `reqwest` GET | Default, cheapest. **Search rides here too:** it GETs DuckDuckGo's server-rendered HTML endpoint through the same proxy-honoring path (a browser-like User-Agent is required, or DDG returns HTTP 202), so search works under the sandbox where a browser can't spawn. |
| **2 · lightpanda** | `agent-browser --engine lightpanda` | JS-rendered pages. `--args ""` is mandatory — Chrome stealth flags break Lightpanda. A per-fetch `--session` id keeps concurrent fetches from sharing cookie jars. |
| **3 · chrome** | `agent-browser --engine chrome` | Anti-bot / screenshot. Stealth flags travel as one `--args "<comma-sep>"` value (bare argv is parsed as a subcommand). A rendered `fetch` escalates lightpanda → chrome on an `Http` failure *or* an empty render; `chrome:true` forces tier 3. |

- **SSRF guard (hard, non-configurable)** — refuse any URL whose resolved IP is private / link-local / loopback / unique-local, screened on every tier. For the browser tiers, the guard and `deny_hosts` are enforced **in gateway code before spawn** — the proxy can't see a browser subprocess's own connections.
- **Content budget** — a single 5 MB page would overflow the worker's context. `fetch` caps returned text at `max_fetch_chars` (default 50 000; `0` disables), on a codepoint boundary, with a truncation marker. The 5 MB body cap bounds transfer/memory; `max_fetch_chars` bounds context. Search snippets aren't capped.
- **Search reliability** — under N concurrent workers DDG rate-limits with a 202 challenge. `search` retries on 202 with exponential backoff + a **query-seeded jitter** — distinct sub-questions stagger their retries instead of re-bursting in sync.
- **URL-level audit** — every call logs `{worker, url, tier, outcome}` to the channel and telemetry. Every report citation reconciles to one gateway audit record.

### Render engine (experimental, opt-in)

Select via `MUR_RESEARCH_RENDER_ENGINE` env (or `research_gateway.render_engine:` in `~/.mur/config.yaml`):

- **`agent-browser` (default)** — Lightpanda (tier 2) and Chrome (tier 3) as above.
- **`obscura` (experimental, opt-in)** — Embedded-V8, self-contained. Install: extract platform tarball to `~/.mur/aura/`, keep both `obscura` and `obscura-worker` binaries. Single render path; no tier-2/3 escalation. **Advantage:** egress is **proxy-governed** — routes through the tier-1 loopback proxy (`obscura fetch <url> … --proxy http://<token>:@127.0.0.1:<port>`), eliminating the browser-tier egress-governance gap. Experimental, not yet default; head-to-head evaluation vs Lightpanda gates default flip.

---

## §5 · Security model — sandbox, consent, and the tool policy

The gateway runs under an enforced kernel sandbox with no default egress. Access is granted through exactly one explicit consent step; the worker's tool policy is shaped so headless turns can neither escape nor stall.

| Control | Mechanism | Boundary |
|---------|-----------|----------|
| **Egress grant** | Per-worker `mcp set-network research-gateway --broad-audited` — one operator consent, recorded as `EgressAuthorization`. | Fleet creation **never** opens egress implicitly. "It's a research fleet" is not consent. |
| **Egress proxy** | One loopback CONNECT proxy; each worker's gateway child gets an `HTTPS_PROXY` token scoped to its allow/deny policy. | Must start **before** the sandbox seals, with its port carved in as a loopback-only rule — else children can't dial it. |
| `mcp__research-gateway__*` | **allow** | Pre-approved so headless fleet turns skip the HITL gate (which has no answerer → 300 s timeout → fail). Grants no egress by itself. |
| `bash` · `read_file` · `write_file` · `edit_file` | **deny** | A research turn that reaches for a built-in would dead-end on the same unanswerable gate. Denied → not advertised → never called. |
| Everything else | **ask** | Fail-closed default preserved for any tool not in the two rules above. |
| **Provenance** | Each worker appends its own reply as an `Agent{self}` event, Ed25519-signed (v3d-2 peer-writes-own), verified per-actor on fold. | The router no longer signs on a worker's behalf — attribution is the worker's own key. |
| **Export safety** | `.fleet` import downgrades broad-audited → `inherit` and clears authorization. | A shared deep-research fleet has zero egress until re-granted locally. |

**Advisory-enforcement honesty.** Tier 1 honors the proxy; the tier 2/3 browser subprocesses may not — mitigated by universal gateway URL audit, and documented rather than overclaimed. With the opt-in `render_engine: obscura`, this gap closes: obscura routes all egress through the loopback proxy tier 1 uses (via `--proxy` with the gateway's credential), making the render tier proxy-governed like tier 1. Airtight containment = a future Phase-3 sbpl pin-to-proxy, which then pins exactly this one gateway.

---

## §6 · Operating it — provision, grant, run

```bash
# 1 · create k restricted workers, each mounting the gateway,
#     and grant broad-audited egress in the same consent step
mur deep-research provision --count 4 --model claude_haiku --grant-egress --yes
#   tool policy: mcp__research-gateway__* → allow
#   tool policy: bash, read_file, write_file, edit_file → deny
#   Updated egress policy for 'research-gateway'.   # × each worker

# 2 · create the fleet (router = mur), set a done marker + budget
mur fleet create deep-research \
    --members dr_worker_1,dr_worker_2,dr_worker_3,dr_worker_4 \
    --goal "…your research question…"
#   fleet.yaml loop: { max_iterations, budget_usd, done_when: marker:RESEARCH_COMPLETE }

# 3 · run the guarded loop — decompose → research → verify → synthesize
mur deep-research run deep-research --max-iterations 4 --deadline 30m
#   fleet 'deep-research' loop stopped after 1 iteration (~$6.14 spent): Converged
```

- **Where results live** — each worker's cited reply is a signed event in `~/.mur/channels/fleet-deep-research/events.jsonl`. The SQLite read-model at `~/.mur/index/channels/` is a rebuildable projection. Every citation reconciles to a gateway audit record.
- **Config knobs (no hardcoded values)** — `MUR_RESEARCH_MAX_FETCH_CHARS`, `…_SEARCH_LIMIT`, `…_TIMEOUT_SECS`, `…_LIGHTPANDA_PATH`, `…_DENY_HOSTS` — env or `research_gateway:` in `~/.mur/config.yaml`, never literals.

---

## §7 · Implementation reality — what the clean design cost to make real

The spec's four-box architecture was right. Getting a fleet to actually converge — sandboxed, headless, cryptographically attributed — surfaced nine distinct fixes, each found by live operator verification, not by reading code.

| PR | Area | Fix |
|----|------|-----|
| **#663** | feat | **Native deep-research core** — the gateway crate, fleet wiring, and router/worker/verify skills. |
| **#664** | HITL | **Pre-approve gateway tools** — headless turns had no one to answer `tool/approval_needed` → 300 s timeout → failed. Stamp `mcp__research-gateway__* → allow` at provision. |
| **#665** | G1 · sandbox | **Pre-seal the egress proxy** — it started *after* the sandbox sealed, on a port never carved in → every scoped grant dead on arrival. Start it pre-seal; carve a loopback-only port rule. |
| **#666** | G3 · channel | **Grant the channel read-model dir** — a signed reply landed in `events.jsonl` but the SQLite refresh hit a read-only DB → false failure. Grant `index/channels`; make the post-append refresh non-fatal. |
| **#667** | G2 · search | **Browserless search + working chrome** — search spawned `agent-browser` the sandbox denies. Route it through tier-1 HTTP; fix the chrome `--args` forwarding so the render fallback actually launches. |
| **#668** | egress | **Case-insensitive `Proxy-Authorization`** — the *total-egress unblock*. The proxy matched the auth header case-sensitively, but hyper emits it lowercase → token dropped → *every* CONNECT denied. Captured live with an `nc` proxy trace. |
| **#669** | content | **Fetch content budget** — one 5 MB page overflowed the worker's context (`anthropic 400: prompt too long`). Cap returned text at `max_fetch_chars`. |
| **#670** | convergence | **Deny worker built-in tools** — the *convergence unblock*. A research turn reached for `bash` (default `ask`) → the unanswerable HITL gate → turn failed → empty reply → step failed. Deny built-ins; the model never sees them. Root-caused via a raw `channel/delegate` socket probe. |
| **#671** | reliability | **DDG 202 retry + jitter** — concurrent workers tripped DuckDuckGo's rate-limit. Retry the 202 with backoff + query-seeded jitter; reports went from "access limited" to 16–19 citations each. |
| **#672** | G4 · cleanup | **Skip non-skill dirs on load** — fleet run-ledgers (`fleet:<name>/`) under `skills/` spammed ~14 name-validation warnings per boot. `load_all` skips manifest-less dirs — in the injection path only, so the maturity sweep is untouched. |

**Verified end to end:** fresh provision → `run`: 0 step failures, all workers wrote **signed** replies (Ed25519 provenance), the loop stopped **Converged**, and the router produced a cited Ollama / LM Studio / LocalAI comparison — under 4-worker concurrency, entirely inside the kernel sandbox.

---

*This design doc reflects the shipped implementation as of v2.45.0. "Airtight" egress containment (sbpl pin-to-proxy) and a first-class search API remain deliberate Phase-3 follow-ons.*
