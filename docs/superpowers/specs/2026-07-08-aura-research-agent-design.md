# AURA — Autonomous Web-Research Agent (Design)

Date: 2026-07-08
Status: Design approved; revised 2026-07-08 (fleet-of-workers → one core, two modes)

## 1. Goal

Perform end-to-end web research: decompose a question, fan out parallel searches,
fetch and render sources (including JavaScript-heavy and login-gated pages), verify
claims adversarially, and return a synthesized report with citations.

**One research core, two delivery modes** (see §4.4):
- **Mode 1 — one-shot public research:** no agent. Invoke the existing `deep-research`
  workflow directly. This is the common case and it sidesteps every per-agent
  sandbox/entitlement cost.
- **Mode 2 — persistent orchestrator:** a single MUR agent `aura` (display **AURA**)
  that wraps Mode 1's core and adds the three things a workflow can't hold —
  authenticated browser state (login), scheduling, and long-term memory.

Large-scale parallelism belongs to the workflow's ephemeral subagents, NOT to a fleet
of persistent agents (the earlier "fleet of workers" shape is dropped — see §4.4).

## 2. Research verdict (what this design is built on)

Deep-research (110-agent fan-out, adversarially verified) established a
three-layer stack. Findings and caveats live in the research transcript; the
load-bearing conclusions:

- **Discovery/search layer** — query decomposition + parallel fan-out + adversarial
  verification is already implemented in MUR's `deep-research` workflow. Reuse it;
  do not rebuild.
- **Fetch/extract layer** — existing `WebFetch` / search tooling covers friendly
  (non-JS, non-auth) pages.
- **Full-browser layer (JS/login)** — resolved to a single swappable control
  surface (see §4.3). Vercel `agent-browser` (Apache-2.0) is the control layer;
  its `--engine` flag swaps the underlying engine between `lightpanda` (low
  footprint) and `chrome` (compatibility).

Tool notes captured during research:

- **Lightpanda** (Zig, V8, no graphics engine): ~24 MB/instance vs Chrome ~2 GB →
  ~8× more parallel workers per box. Beta. Weak against anti-bot fingerprinting; no
  screenshots. AGPL-3.0. **Verified working via agent-browser 0.31.1** with two
  requirements: install the Lightpanda binary separately (agent-browser does not
  bundle it) and reach it via `--executable-path`/`AGENT_BROWSER_EXECUTABLE_PATH`;
  and do NOT forward Chrome-only launch args (pass `--args ""` / `AGENT_BROWSER_ARGS=""`).
- **agent-browser** (vercel-labs, Rust, Apache-2.0): control CLI (`mcp` stdio server
  since v0.28.0), `--engine chrome|lightpanda` (both verified; lightpanda per above),
  per-call/env overrides (`AGENT_BROWSER_ENGINE|EXECUTABLE_PATH|ARGS`), `connect
  <port|wss>` (CDP), `-p <cloud provider>`, auth vault (`auth save/login`, LLM never
  sees passwords). Runs a persistent daemon (`pkill -f agent-browser` to reset after
  an upgrade).
- **Obscura** (h4ckf0r0day/obscura, Rust, Apache-2.0): verified genuine
  (17.9k★, 1.24k forks, 31 contributors, monthly releases) but early (v0.1.x,
  created 2026-04). Watch-list only, not a launch dependency.
- **ego-lite** (citrolabs/ego-lite): real; shared-desktop-Chrome "Spaces" model.
  Best for interactive research using the operator's own Chrome logins — a poor
  fit for a headless N-worker fleet. Out of scope for launch.

## 3. Licensing constraint (hard rule)

Lightpanda is AGPL-3.0. MUR stays proprietary **only** if Lightpanda is invoked
**as a separate subprocess over CDP** (arm's-length), never linked in-process.

- Allowed: MUR → `agent-browser` (Apache-2.0) → `lightpanda` subprocess (a separate
  binary agent-browser launches via `--executable-path`; still a separate process).
- Obligations when shipping: ship the unmodified upstream Lightpanda binary (or a
  link to its source) and include AGPL notice/attribution. Do **not** fork/modify
  Lightpanda.
- Red line: never statically embed the Lightpanda library into any MUR binary.
- Enterprise note: some procurement policies ban AGPL dependencies even at
  arm's-length. If that blocks a customer, fall back to `--engine chrome` (drops
  the AGPL component entirely) and revisit Obscura when it matures.

## 4. Architecture

Core principle (YAGNI): MUR already has the discovery layer (`deep-research`
workflow), the fetch layer (`WebFetch`), and the fleet machinery. The only new
capability is the full-browser layer. Build that; reuse the rest.

### 4.1 Agent profile

```
name:         aura            # lowercase — matches on-disk dir + runtime spoof check
display_name: AURA            # uppercase per brand rule
role:         autonomous web researcher — end-to-end, cited reports
model_ref:    <a strong-reasoning model alias from ~/.mur/models.yaml>
```

### 4.2 Three-layer research flow

- **(a) Discovery** — `aura` invokes the existing `deep-research` workflow for
  decomposition, parallel search, adversarial verification, synthesis. No rewrite.
  This is ALSO the scale layer: large fan-out (100+ parallel research units) happens
  inside the workflow's **ephemeral subagents** (bounded concurrency ~min(16, cores-2),
  no per-agent process/sandbox/entitlement), NOT by adding fleet members. A worker that
  receives a broad slice calls `deep-research` rather than spawning more agents.
- **(b) Fetch** — existing `WebFetch` / search tools for plain pages.
- **(c) Full-browser** — new: `agent-browser` wired as an MCP tool, default engine
  `lightpanda` (~24MB, verified) via env `AGENT_BROWSER_ENGINE=lightpanda` +
  `AGENT_BROWSER_EXECUTABLE_PATH=<lightpanda>` + `AGENT_BROWSER_ARGS=""`; chrome as
  fallback for anti-bot/screenshot pages. Credentials go through agent-browser's
  auth vault (never plaintext, never in the LLM context).

### 4.3 Escalation ladder (single control surface)

One tool, engine/provider chosen per page difficulty:

```
WebSearch / WebFetch           # plain text — cheapest
   ↓  (page needs JS)
agent-browser --engine lightpanda   # JS rendering, ~24MB — DEFAULT browser tier (verified)
   ↓  (anti-bot fingerprint wall, screenshot, or lightpanda renders wrong)
agent-browser --engine chrome       # full Chrome compatibility (+ stealth args)
   ↓  (need scale/anti-bot beyond one box — future, no code change)
agent-browser -p kernel|browserbase # cloud providers
```

Switching tiers is a flag change; the escalation logic lives in a skill (§4.5).

### 4.4 Two delivery modes (no fleet)

The "fleet of aura workers" is dropped. The `deep-research` workflow already does
decomposition + parallel fan-out + adversarial verification + synthesis better than a
router splitting work across a few agents, and its ephemeral subagents avoid the
per-agent sandbox/entitlement cost entirely (they run as host-level workflow subagents
using `WebSearch`/`WebFetch`, not launchd `mur-agent-runtime` processes). So parallelism
= the workflow; the agent, when present, is a thin persistent shell.

**Mode 1 — one-shot public research (the common case).**
- No agent, no fleet. Invoke the `deep-research` workflow with the question.
- Covers public + `WebFetch`-renderable pages, at whatever scale the workflow's bounded
  concurrency (~min(16, cores-2)) supports — 100+ sub-questions run as ephemeral
  subagents, never as persistent agents.
- **Sidesteps the network-entitlement gap entirely** (§ Runtime gaps): workflow
  subagents are not per-agent-sandboxed. Ships today.

**Mode 2 — persistent orchestrator `aura` (login / schedule / memory).**
- ONE agent `aura` (not a squad). Its job is to orchestrate, not to out-parallelize the
  workflow: it calls `deep-research` for volume (public pages) and uses its own browser
  tier (§4.3) only for the pages a workflow can't reach — **login-gated / heavy-JS**.
- Adds exactly what a workflow can't hold: **authenticated browser state** (agent-browser
  auth vault, persisted across runs), **scheduling** (`mur agent schedule` — nightly /
  triggered research), and **long-term memory** (accumulate/dedup across sessions).
- Gap footprint is a single controlled point: only `aura`'s own login-gated fetches
  touch the per-agent network entitlement — exactly where audited/scoped egress is
  wanted anyway. Public volume rides the un-sandboxed workflow.

**When a real fleet is justified (long tail, not this design):** heterogeneous
persistent specialists under cryptographic governance/audit (Commander). Not built here.

### 4.5 Skills (Mode 2)

Reusable patterns from the research, authored as `aura` skills. Since there is no
fleet, scope them to the **User** (they inject for the `aura` agent's sessions), not
Fleet:

1. `browser-preflight` — before the first browser-tier call, detect whether
   `agent-browser` and the Lightpanda engine are installed. If missing, ask the
   operator for permission and only then install; never auto-install. If declined,
   degrade to the fetch tier and report what could not be reached.
2. `research-escalation-ladder` — climb search → fetch → lightpanda → chrome only
   when the cheaper tier fails; never open a browser for a page plain fetch can read.
3. `source-triangulation` — cross-check each claim across ≥2 independent sources;
   surface and resolve conflicts rather than picking one silently.
4. `citation-discipline` — bind every claim to a fetched URL + supporting quote;
   an unsourced claim is dropped, not shipped.
5. `research-mode-select` — decide Mode 1 (call `deep-research` for public volume) vs
   Mode 2 browser tier (login-gated / heavy-JS pages the workflow can't reach).

Install consent is a hard rule: installing software is a permission-required action
(CLAUDE.md / safety policy). The preflight skill asks; it never installs silently.

## 5. Component boundaries

| Unit | Responsibility | Depends on |
|------|----------------|------------|
| `deep-research` workflow | Discovery + parallel fan-out + verification + synthesis (both modes) | (existing) |
| `aura` agent profile (Mode 2) | Identity, auth vault, schedule, memory, tool/skill wiring | MUR runtime |
| `agent-browser` tool (Mode 2) | Login-gated / heavy-JS fetch/render | subprocess; Lightpanda/Chrome |
| `aura` skills (×5) | When/why to escalate, triangulate, cite, pick mode | injected at session |

Each unit is independently testable: Mode 1 runs with no agent (invoke the workflow);
the agent boots without the browser tool; skills inject without the browser present;
the browser tool is exercised standalone via its CLI.

## 6. Open questions (resolve in the implementation plan)

1. **Integration surface** — RESOLVED: `agent-browser mcp` is a stdio MCP server
   (>= 0.28.0); wired via `mur agent mcp add`.
2. **Mode-1 heavy-JS reach** — the workflow uses `WebFetch` (static). Confirm whether
   heavy-JS *public* pages need escalation to Mode 2, or whether a browser MCP can be
   made available inside the workflow.
3. **Model choice** — RESOLVED for Mode 2: `claude_sonnet` for `aura`.
4. **Credential provisioning** — how operators load site logins into agent-browser's
   auth vault without those secrets touching MUR/LLM context (Mode 2, login case).

## 7. Out of scope (launch)

- **Fleet of persistent worker agents** — dropped; parallelism = the workflow (§4.4).
- Obscura as an engine (watch-list; revisit at v0.2+/v1).
- ego-lite integration (interactive, operator-Chrome model).
- Remote `--cdp wss://` cloud endpoints (agent-browser support still maturing).
- Any in-process embedding of a browser engine (AGPL red line).
- Governance/audit multi-agent squads (Commander long tail).
