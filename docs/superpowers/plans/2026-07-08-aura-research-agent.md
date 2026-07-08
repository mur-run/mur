# AURA Autonomous Web-Research Agent — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up `aura`, a MUR agent that runs end-to-end web research as a fleet of parallel workers, using `agent-browser` (default `--engine lightpanda`, ~24MB; chrome fallback) for JS/login pages.

**Architecture:** AURA is composed from existing MUR primitives, not new runtime code: a `mur agent` profile + the `agent-browser` MCP tool + four agent-scoped skills + a `mur fleet`. The discovery/verification layer reuses the existing `deep-research` workflow; the full-browser layer is a swappable `agent-browser` control surface.

**Tech Stack:** MUR CLI (`mur agent`, `mur skill`, `mur fleet`), `agent-browser` (npm, Apache-2.0), Lightpanda engine (AGPL-3.0, subprocess-only).

## Global Constraints

- Brand name user-facing is uppercase **AURA**; internal `name` is lowercase `aura` (matches on-disk dir + runtime spoof check). — spec §4.1, CLAUDE.md rule 7.
- Lightpanda (AGPL-3.0) may only be invoked as a **separate subprocess over CDP** — never linked in-process, never forked/modified. Ship unmodified upstream binary + AGPL attribution. — spec §3.
- `agent-browser` is the single control surface; engine/args/executable are set via env (`AGENT_BROWSER_ENGINE`, `AGENT_BROWSER_ARGS`, `AGENT_BROWSER_EXECUTABLE_PATH`) or per-call flags, fleet logic unchanged. `--engine lightpanda` requires the Lightpanda binary installed separately + `AGENT_BROWSER_ARGS=""` (Chrome stealth args must not reach lightpanda). — spec §4.3, verified 0.31.1.
- Fleet safety triad reused unchanged: `MUR_FLEET_AUTORUN` off by default + per-fleet `budget_usd` + `mur fleet stop` kill-switch; steps pass `yes:false` (fail-closed). — spec §4.4.
- No hardcoded values: model alias comes from `~/.mur/models.yaml` (operator selects), never hardcoded. — CLAUDE.md rule 1.
- This plan writes configuration + skill YAML only. It does not add Rust code. If any task appears to need a new MUR runtime feature, STOP and raise it — that is a separate spec.

---

### Task 1: Provision and verify `agent-browser` + Lightpanda engine (standalone)

Prove the browser layer works in isolation before wiring it into MUR. If this fails, nothing downstream can work.

**Files:**
- Create: none (external tool install)
- Verify: `~/.mur/aura/PREREQS.md` (record installed versions + AGPL attribution note)

**Interfaces:**
- Produces: a working `agent-browser` binary (>= 0.28.0) on PATH with `agent-browser mcp` stdio server, plus the Lightpanda binary at `~/.mur/aura/lightpanda`; consumed by Task 3.

- [ ] **Step 1: Install agent-browser and its browser assets**

```bash
npm i -g agent-browser
agent-browser install          # downloads Chrome-for-Testing (chrome engine fallback)
agent-browser --version
```
Expected: `agent-browser --version` is **>= 0.28.0** (the `mcp` subcommand landed in
0.28.0; 0.27.x lacks it). Force latest if a stale global copy resolves lower:
`npm i -g agent-browser@latest`. Verified good on 0.31.1.

- [ ] **Step 2: Install the Lightpanda binary (consent-gated) and verify it renders**

agent-browser does NOT bundle Lightpanda. Installing is permission-required (Global
Constraints) — on explicit operator yes, on macOS Apple Silicon:
```bash
mkdir -p ~/.mur/aura
curl -fL -o ~/.mur/aura/lightpanda https://github.com/lightpanda-io/browser/releases/download/nightly/lightpanda-aarch64-macos
chmod a+x ~/.mur/aura/lightpanda
~/.mur/aura/lightpanda version    # e.g. 1.0.0-nightly.7813+...
```
Then verify the engine renders (note `--args ""` — Chrome stealth args must not reach lightpanda):
```bash
agent-browser --engine lightpanda --executable-path ~/.mur/aura/lightpanda --args "" open https://example.com snapshot
```
Expected: `✓ Example Domain` + the URL. VERIFIED working on 0.31.1 (2026-07-08). If the
operator declines the install, the browser tier falls back to `--engine chrome` (Step 2b).

- [ ] **Step 2b: Verify the chrome fallback engine**

```bash
agent-browser --engine chrome open https://example.com snapshot
```
Expected: a snapshot prints. Chrome is the fallback for anti-bot/screenshot pages and the default if Lightpanda is not installed.

- [ ] **Step 3: Verify concurrent isolated sessions**

Launch two named sessions and confirm they are isolated (own cookies/state):
```bash
agent-browser --engine lightpanda --executable-path ~/.mur/aura/lightpanda --args "" --session s1 open https://example.com snapshot &
agent-browser --engine lightpanda --executable-path ~/.mur/aura/lightpanda --args "" --session s2 open https://example.org snapshot &
wait
agent-browser close --all
```
Expected: both complete without cross-talk; two distinct snapshots. This validates the per-worker isolation the fleet depends on (spec §4.4).

- [ ] **Step 4: Verify the MCP stdio server starts**

```bash
agent-browser mcp --tools core <<< '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}'
```
Expected: a JSON-RPC initialize response on stdout (confirms the stdio server MUR will spawn). Requires >= 0.28.0.

- [ ] **Step 5: Record versions + AGPL attribution, commit**

Write `~/.mur/aura/PREREQS.md` with the `agent-browser --version` output, the Lightpanda binary version, and a one-line AGPL-3.0 attribution/notice for Lightpanda (spec §3). Then:
```bash
git add docs/superpowers/plans/2026-07-08-aura-research-agent.md
git commit -m "chore(aura): record browser-layer prereqs and AGPL attribution"
```

---

### Task 2: Create the `aura` agent profile

**Files:**
- Create: `~/.mur/agents/aura/` (via CLI) — profile, identity
- Reference: `mur-core/src/cmd/agent/lifecycle.rs` (create command)

**Interfaces:**
- Consumes: a model alias present in `~/.mur/models.yaml`.
- Produces: a bootable agent named `aura` (display `AURA`); consumed by Tasks 3–5.

- [ ] **Step 1: Choose a model alias (no hardcoding)**

```bash
mur model list
```
Pick a strong-reasoning alias for the router/members (record the chosen alias; it is the operator's decision per Global Constraints). Export it for the next step:
```bash
export AURA_MODEL=<chosen-alias-from-list>
```
Expected: `mur model list` shows at least one usable alias; `$AURA_MODEL` is set.

- [ ] **Step 2: Create the agent**

```bash
mur agent create aura --no-interactive --display-name AURA --model "$AURA_MODEL"
```
Expected: agent `aura` created at `~/.mur/agents/aura/`.

KNOWN MUR BUG (verified 2026-07-08, aura build): `mur agent create --model <alias>`
does NOT set `model_ref` — it stores the alias as a literal inline block
`provider: ollama / name: <alias>`, so the registry alias never resolves and the
runtime falls back to a nonexistent ollama model (runtime prefers `profile.model_ref`
— `mur-agent-runtime/src/supervisor.rs:1121`). Fix immediately after create: edit
`~/.mur/agents/aura/profile.yaml` to add `model_ref: <alias>` and sync the inline
block to the alias's real resolution from `mur model show <alias>`:
```yaml
model:
  provider: anthropic      # from `mur model show claude_sonnet`
  name: claude-sonnet-5
  params: {}
model_ref: claude_sonnet
```
Verify with `mur agent status aura` and `mur agent card aura` (there is NO
`mur agent doctor <name>` — `doctor` takes no agent arg). This same fix applies to
every worker clone in Task 5.

- [ ] **Step 3: Set the role/system prompt**

```bash
mur agent prompt set aura "You are AURA, an autonomous web researcher. You decompose questions, fan out parallel searches, fetch and render sources (including JS/login pages via the browser tool), verify each claim against >=2 independent sources, and return a synthesized report where every claim is bound to a source URL and quote. Escalate search -> fetch -> browser only when the cheaper tier fails."
```
Expected: prompt written. Verify with `mur agent prompt show aura`.

- [ ] **Step 4: Verify the agent boots**

```bash
mur agent status aura      # ● aura - custom, Active: stopped
mur agent card aura        # valid A2A card: name=aura, displayName=AURA
mur agent list             # aura appears
```
Expected: profile loads cleanly, model_ref resolves. NOTE: `mur agent doctor <name>` does NOT exist — `doctor` takes no agent arg (validates export tooling only). Use `status`/`card`.

- [ ] **Step 5: Commit the config snapshot**

The agent lives at `~/.mur/agents/aura/` — OUTSIDE the repo. There is nothing to
commit unless you export an artifact into the repo. `mur agent export` produces a
non-deterministic signed binary `.muragent` (not worth committing). So this step is
typically a no-op: confirm `git status --short` is empty and skip the commit rather
than fake one. (Optionally record a short profile summary by hand into
`docs/superpowers/plans/artifacts/aura-profile.md` and commit that.)

---

### Task 3: Wire `agent-browser` MCP tool into `aura`

**Files:**
- Modify: `~/.mur/agents/aura/` profile `mcp_servers` (via CLI)
- Reference: `mur-core/src/cmd/agent/mcp.rs` (add command + spawn allowlist)

**Interfaces:**
- Consumes: `agent-browser` + Lightpanda binary from Task 1; `aura` agent from Task 2.
- Produces: `aura` has two browser tools — `agent-browser` (lightpanda, default) and `agent-browser-chrome` (fallback); consumed by Task 6.

- [ ] **Step 1: Add the lightpanda browser tool (default, low-footprint)**

Engine/executable/args are global flags placed BEFORE the `mcp` subcommand (verified:
they apply to the spawned server). `--args ""` stops the global Chrome stealth args
from reaching Lightpanda.
```bash
mur agent mcp add aura agent-browser --command agent-browser \
  --arg --engine --arg lightpanda \
  --arg --executable-path --arg "$HOME/.mur/aura/lightpanda" \
  --arg --args --arg "" \
  --arg mcp --arg --tools --arg core
mur agent mcp list aura
```
Expected: `agent-browser` listed with the lightpanda flags + `mcp --tools core`, command resolved on PATH and pinned (see `mcp.rs`).

- [ ] **Step 2: Add the chrome browser tool (fallback for anti-bot/screenshots)**

The MCP tools don't expose per-call engine switching, so the chrome fallback is a
second server. It inherits the global stealth args (no `--args ""`).
```bash
mur agent mcp add aura agent-browser-chrome --command agent-browser \
  --arg --engine --arg chrome \
  --arg mcp --arg --tools --arg core
mur agent mcp list aura
```
Expected: both `agent-browser` (lightpanda) and `agent-browser-chrome` listed. The `aura-research-escalation-ladder` skill (Task 4) tells the agent to prefer the lightpanda tool and fall back to chrome only for anti-bot/screenshot pages.

- [ ] **Step 3: Verify both tools are visible to the agent**

```bash
mur agent doctor aura
```
Expected: both MCP servers listed and their commands resolve. No spawn-allowlist violation.

- [ ] **Step 4: Verify the agent can drive a JS page end-to-end (lightpanda)**

Use an interactive session to make `aura` fetch a JS-rendered page through the tool:
```bash
mur agent cli aura
# then prompt: "Use the browser tool to open https://example.com and give me the page's main heading text."
```
Expected: `aura` calls the lightpanda tool and returns the heading. This proves MUR -> agent-browser -> lightpanda works (spec §4.3). Note: tool-executing turns require `mur agent cli`, NOT `mur agent send` (send has no tool execution and auto-denies HITL headless — see mem:gotcha_mur_agent_send_no_tools).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(aura): wire agent-browser MCP tools (lightpanda default + chrome fallback)"
```

---

### Task 4: Author the five AURA-scoped skills

**Files:**
- Create: `~/.mur/skills/aura-browser-preflight/skill.yaml`
- Create: `~/.mur/skills/aura-research-escalation-ladder/skill.yaml`
- Create: `~/.mur/skills/aura-source-triangulation/skill.yaml`
- Create: `~/.mur/skills/aura-citation-discipline/skill.yaml`
- Create: `~/.mur/skills/aura-parallel-fanout/skill.yaml`
- Reference: `mur-core/src/cmd/skill_cmd.rs` (`cmd_new` scaffold + scope subcommand; `SkillScope`)

**Interfaces:**
- Consumes: nothing (pure knowledge objects).
- Produces: five skills injected into `aura` sessions; consumed at runtime in Task 6.

- [ ] **Step 1: Scaffold the five skills**

```bash
mur skill new aura-browser-preflight
mur skill new aura-research-escalation-ladder
mur skill new aura-source-triangulation
mur skill new aura-citation-discipline
mur skill new aura-parallel-fanout
```
Expected: five `skill.yaml` files scaffolded under `~/.mur/skills/`.

- [ ] **Step 2: Fill in the browser-preflight skill content**

Edit `~/.mur/skills/aura-browser-preflight/skill.yaml` so `content` teaches: BEFORE the first browser-tier call in a research job, detect the toolchain, and if anything is missing, ask the operator for permission and only then install — never install silently. Triggers: about to use the browser tool, JS/login page needed, start of a research job. The detection + consent procedure to encode:

```bash
# detect the control layer (needs >= 0.28.0 for the mcp server)
agent-browser --version || MISSING_TOOL=1
# detect the lightpanda binary (the low-footprint default engine)
[ -x "$HOME/.mur/aura/lightpanda" ] || MISSING_LIGHTPANDA=1
# detect the chrome fallback engine (installed by `agent-browser install`)
agent-browser --engine chrome open about:blank snapshot >/dev/null 2>&1 || MISSING_CHROME=1
```
If `MISSING_TOOL`: STOP and ask, e.g. "agent-browser isn't installed. May I run `npm i -g agent-browser@latest && agent-browser install`?" If `MISSING_LIGHTPANDA`: ask, e.g. "the Lightpanda engine (low-footprint default) isn't installed. May I download it to `~/.mur/aura/lightpanda`?" (curl per Task 1 Step 2). Install ONLY on an explicit yes. If both browser engines are unavailable and the operator declines, degrade to the fetch tier (`WebFetch`) and report which pages could not be rendered. If only Lightpanda is missing, chrome is a working fallback. Installing software is a permission-required action (Global Constraints) — the skill asks, it never auto-installs.

- [ ] **Step 3: Fill in the escalation-ladder skill content**

Edit `~/.mur/skills/aura-research-escalation-ladder/skill.yaml` so `content` teaches: try `WebSearch`/`WebFetch` first; escalate to the lightpanda browser tool (`agent-browser`, low-footprint) only when a page needs JS or a login; escalate to the chrome tool (`agent-browser-chrome`) only for anti-bot fingerprint walls, screenshots, or when lightpanda renders the page wrong. Triggers: research, fetch a page, JS-heavy site, login-gated. Never open a browser for a page plain fetch can read. (Content is the spec §4.3 ladder in prose.)

- [ ] **Step 4: Fill in triangulation, citation, fanout skills**

- `aura-source-triangulation`: cross-check each claim across >=2 independent sources; surface and resolve conflicts explicitly rather than silently picking one. Trigger: verifying a claim.
- `aura-citation-discipline`: bind every claim to a fetched URL + supporting quote; drop any claim with no source rather than shipping it. Trigger: writing the report.
- `aura-parallel-fanout`: broad decomposable question -> spin up the fleet; narrow question -> single-agent concurrent fetches. Trigger: deciding how to run a research job.

- [ ] **Step 5: Scope all five to the agent/fleet and verify injection**

```bash
mur skill scope aura-browser-preflight --fleet
mur skill scope aura-research-escalation-ladder --fleet
mur skill scope aura-source-triangulation --fleet
mur skill scope aura-citation-discipline --fleet
mur skill scope aura-parallel-fanout --fleet
```
Expected: each `skill.yaml` `scope:` becomes `Fleet` (see `skill_cmd.rs` scope mapping / `SkillScope::Fleet`). Verify they retrieve for a research query:
```bash
mur skill list | grep aura-
```
Expected: all five present.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(aura): author 5 research skills (preflight, escalation, triangulation, citation, fanout)"
```

---

### Task 5: Create the `aura-research` fleet

**Files:**
- Create: `~/.mur/fleets/aura-research/fleet.yaml` (via CLI) + shared channel `fleet-aura-research`
- Reference: `mur-core/src/cmd/fleet/create.rs` (`cmd_fleet_create`)

**Interfaces:**
- Consumes: the `aura` agent (Task 2). For real member fan-out, additional runnable member agents are needed (see Step 2).
- Produces: a runnable fleet with router + members over one signed channel; consumed by Task 6.

- [ ] **Step 1: Create the fleet with a goal**

```bash
mur fleet create aura-research --goal "Answer the operator's research question with a cited, verified report."
mur fleet show aura-research
```
Expected: `~/.mur/fleets/aura-research/fleet.yaml` written; router + channel `fleet-aura-research` present (see `create.rs`). Fleet name validated as a lowercase slug.

- [ ] **Step 2: Add members (aura workers)**

The fleet needs >=2 runnable member agents for parallel fan-out. Create worker clones from the `aura` profile (same model, prompt, browser tool, skills), e.g. `aura-w1`, `aura-w2`, then register them as members. Use `mur fleet show aura-research` to confirm roles (router->Router, members->Delegate).
```bash
# repeat Task 2 + Task 3 wiring for each worker, or export/import the aura profile:
mur agent export aura --out /tmp/aura.muragent && mur agent import /tmp/aura.muragent --as aura-w1
mur agent import /tmp/aura.muragent --as aura-w2
# then add them to the fleet per `mur fleet --help` (create/edit membership)
```
Expected: two members visible in `mur fleet show aura-research`. (If the installed `mur fleet create` takes `--members` directly, prefer that single command — check `mur fleet create --help`.)

- [ ] **Step 3: Set the safety guards**

```bash
# budget is required for any future auto-run; keep autorun OFF (default)
mur fleet show aura-research      # confirm no autorun trigger set
```
Set `loop.budget_usd` in `fleet.yaml` to a small positive ceiling (e.g. a few dollars) so a future `--loop` or auto-run can never exceed it (spec §4.4). Do NOT set `MUR_FLEET_AUTORUN`.
Expected: `budget_usd > 0`, no `loop.trigger`.

- [ ] **Step 4: Verify one iteration runs (fail-closed)**

```bash
mur fleet run aura-research
```
Expected: the router fans the goal to members via the DAG executor; each member runs one turn; run completes without blanket-approving any risk-tiered step (`yes:false`). Requires the member agents to be running (operator-tested).

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(aura): create aura-research fleet with budget guard"
```

---

### Task 6: End-to-end acceptance — a real research question

Prove the whole system produces a cited, verified report and that the escalation ladder and kill-switch behave.

**Files:**
- Create: `docs/superpowers/plans/artifacts/aura-e2e-report.md` (captured output)

**Interfaces:**
- Consumes: everything from Tasks 1–5.
- Produces: acceptance evidence.

- [ ] **Step 1: Run a JS/login-requiring research question through the fleet**

Pick a question that forces at least one JS-rendered page (so the browser tier is exercised). Run:
```bash
mur fleet run aura-research    # with the question as the goal/input per `mur fleet run --help`
```
Expected: a synthesized answer where claims carry source URLs (citation-discipline skill), and at least one source was fetched via `agent-browser` (escalation-ladder skill fired). Capture output to the artifact file.

- [ ] **Step 2: Verify citation discipline held**

Inspect the report: every non-trivial claim has a URL + quote; no unsourced claims shipped. If any claim is unsourced, the `aura-citation-discipline` skill needs stronger content — fix and re-run.
Expected: PASS = zero unsourced claims.

- [ ] **Step 3: Verify the escalation ladder did not over-escalate**

Check the run's tool calls (channel events / transcript): plain pages were read via `WebFetch`, only JS pages hit `agent-browser`. A browser call for a page plain fetch could read = ladder violation; strengthen the skill.
Expected: PASS = no gratuitous browser calls.

- [ ] **Step 4: Verify the kill-switch**

```bash
mur fleet stop aura-research
mur fleet run aura-research      # must refuse
mur fleet start aura-research    # clears the sentinel
```
Expected: `run` refuses while stopped (`.stopped` sentinel), then works after `start`. Confirms the kill-switch (spec §4.4).

- [ ] **Step 5: Commit acceptance evidence**

```bash
git add docs/superpowers/plans/artifacts/aura-e2e-report.md
git commit -m "test(aura): end-to-end acceptance — cited report, ladder, kill-switch verified"
```

---

## Self-Review

**Spec coverage:**
- §2 three-layer stack → Tasks 1 (browser), 3 (wiring), 6 (discovery via fleet run). Discovery reuses `deep-research` (no task needed — existing).
- §3 AGPL subprocess/attribution → Task 1 Step 5 + Global Constraints.
- §4.1 profile (lowercase name / uppercase display) → Task 2.
- §4.2 three-layer flow → Tasks 2–5.
- §4.3 escalation ladder → Task 3 (engine default) + Task 4 skill + Task 6 Step 3 verification.
- §4.4 fleet + safety triad → Task 5 + Task 6 Step 4.
- §4.5 five skills (incl. `browser-preflight` install-consent) → Task 4.
- §6 open questions: #1 integration surface RESOLVED (agent-browser mcp stdio, Task 3); #2 worker ceiling → Task 1 Step 3 + set `max_concurrency` during Task 5 (flagged); #3 model choice → Task 2 Step 1; #4 credential provisioning → deferred (Task 3 uses no-auth pages; vault provisioning is a follow-on, noted below).
- §7 out of scope respected (no Obscura/ego-lite/remote-cdp/embedding tasks).

**Placeholder scan:** Model alias and question inputs are operator decisions with concrete listing commands (`mur model list`), not placeholders. CLI flag mismatches are handled with explicit "run `--help` and map" fallbacks rather than guesses (CLAUDE.md rule 2).

**Type consistency:** Names consistent across tasks — agent `aura`, workers `aura-w1/-w2`, fleet `aura-research`, channel `fleet-aura-research`, five skills `aura-*`, two browser tools `agent-browser` (lightpanda) + `agent-browser-chrome`.

**Known follow-on (not in this plan):** credential-vault provisioning for login-gated sites (spec §6 #4) — introduce once no-auth E2E passes; it needs a decision on how operators load secrets into agent-browser's vault without touching MUR/LLM context.
