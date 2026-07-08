# AURA — Autonomous Web-Research Agent (Design)

Date: 2026-07-08
Status: Design approved, pending implementation plan

## 1. Goal

Create a MUR agent, internal name `aura` (display name **AURA**), that performs
end-to-end web research: decompose a question, fan out parallel searches, fetch
and render sources (including JavaScript-heavy and login-gated pages), verify
claims adversarially, and return a synthesized report with citations.

Parallelism runs as a **MUR fleet of `aura` workers** over one signed channel.

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
  ~8× more parallel workers per box. CDP server, MCP support. Beta. Weak against
  anti-bot fingerprinting; no screenshots. AGPL-3.0.
- **agent-browser** (vercel-labs, Rust, Apache-2.0): control CLI with
  `--engine chrome|lightpanda`, `--cdp <port|wss>`, `-p <cloud provider>`,
  encrypted credential vault (LLM never sees passwords).
- **Obscura** (h4ckf0r0day/obscura, Rust, Apache-2.0): verified genuine
  (17.9k★, 1.24k forks, 31 contributors, monthly releases) but early (v0.1.x,
  created 2026-04). Watch-list only, not a launch dependency.
- **ego-lite** (citrolabs/ego-lite): real; shared-desktop-Chrome "Spaces" model.
  Best for interactive research using the operator's own Chrome logins — a poor
  fit for a headless N-worker fleet. Out of scope for launch.

## 3. Licensing constraint (hard rule)

Lightpanda is AGPL-3.0. MUR stays proprietary **only** if Lightpanda is invoked
**as a separate subprocess over CDP** (arm's-length), never linked in-process.

- Allowed: MUR → `agent-browser` (Apache-2.0) → `lightpanda` subprocess.
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
- **(b) Fetch** — existing `WebFetch` / search tools for plain pages.
- **(c) Full-browser** — new: `agent-browser` wired as a subprocess/MCP tool,
  default `--engine lightpanda`, for JS/login pages. Credentials go through
  agent-browser's encrypted vault (never plaintext, never in the LLM context).

### 4.3 Escalation ladder (single control surface)

One tool, engine/provider chosen per page difficulty:

```
WebSearch / WebFetch           # plain text — cheapest
   ↓  (page needs JS)
agent-browser --engine lightpanda   # JS rendering, low footprint — default browser tier
   ↓  (anti-bot fingerprint wall, screenshot, or operator's private login)
agent-browser --engine chrome       # full Chrome compatibility
   ↓  (need scale/anti-bot beyond one box — future, no code change)
agent-browser -p kernel|browserbase # cloud providers
```

Switching tiers is a flag change; fleet orchestration logic is unchanged.

### 4.4 Fleet design

- `mur fleet create aura-research`.
  - Router = one `aura` (decompose → route via DAG plan → merge).
  - Members = N `aura` clones, each owning a sub-question.
  - Shared signed channel `fleet-aura-research` (router→Router, members→Delegate).
- Parallel isolation: each worker runs its own `agent-browser --session <id>`
  instance (isolated cookies/auth), matching the "per-agent profile isolation to
  avoid session collisions" finding.
- Worker density: with `--engine lightpanda` (~24 MB/instance), a single box hosts
  far more concurrent workers than a Chrome-based fleet. Concrete ceiling per host
  is left to the implementation plan (open question §6).
- Safety triad reused unchanged: `MUR_FLEET_AUTORUN` (off by default) + per-fleet
  `budget_usd` + `mur fleet stop` kill-switch. Steps pass `yes:false` (fail-closed).

### 4.5 Skills wired to `aura`

Reusable patterns from the research, authored as `aura`-scoped skills (agent-scope):

1. `browser-preflight` — before the first browser-tier call, detect whether
   `agent-browser` and both engines (`lightpanda`, `chrome`) are installed. If any
   is missing, ask the operator for permission and only then install
   (`npm i -g agent-browser && agent-browser install`); never auto-install. If the
   operator declines, degrade to the fetch tier and report what could not be reached.
2. `research-escalation-ladder` — climb search → fetch → lightpanda → chrome only
   when the cheaper tier fails; never open a browser for a page plain fetch can read.
3. `source-triangulation` — cross-check each claim across ≥2 independent sources;
   surface and resolve conflicts rather than picking one silently.
4. `citation-discipline` — bind every claim to a fetched URL + supporting quote;
   an unsourced claim is dropped, not shipped.
5. `parallel-fanout` — decide when to spin up the fleet vs. run single-agent
   concurrent fetches (broad, decomposable question → fleet; narrow → single).

Install consent is a hard rule: installing software is a permission-required action
(CLAUDE.md / safety policy). The preflight skill asks; it never installs silently.

## 5. Component boundaries

| Unit | Responsibility | Depends on |
|------|----------------|------------|
| `aura` agent profile | Identity, model, entitlements, skill/tool wiring | MUR runtime |
| `deep-research` workflow | Discovery + verification + synthesis | (existing) |
| `agent-browser` tool | Full-browser fetch/render/auth | subprocess; Lightpanda/Chrome |
| `aura-research` fleet | Parallel decomposition + merge | fleet executor, signed channel |
| `aura` skills (×4) | When/why to escalate, triangulate, cite, fan out | injected at session |

Each unit is independently testable: the agent boots without the fleet; the fleet
runs with stubbed members; skills inject without the browser tool present; the
browser tool is exercised standalone via its CLI.

## 6. Open questions (resolve in the implementation plan)

1. **Integration surface** — does `agent-browser` expose an MCP server, or is it
   wired as a command/CLI tool? Verify before choosing the wiring mechanism.
2. **Per-host worker ceiling** — measure real RAM/CPU per `--engine lightpanda`
   session to size the fleet; set a default `max_concurrency`.
3. **Model choice** — which `models.yaml` alias for router vs. member (router may
   warrant a stronger model than members).
4. **Credential provisioning** — how operators load site logins into
   agent-browser's vault without those secrets touching MUR/LLM context.

## 7. Out of scope (launch)

- Obscura as an engine (watch-list; revisit at v0.2+/v1).
- ego-lite integration (interactive, operator-Chrome model; not fleet-shaped).
- Remote `--cdp wss://` cloud endpoints (agent-browser support still maturing).
- Any in-process embedding of a browser engine (AGPL red line).
