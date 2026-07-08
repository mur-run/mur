# AURA Autonomous Web-Research Agent — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **⚠ REVISED 2026-07-08 — fleet dropped; one core, two modes.** The live E2E showed
> the multi-worker fleet is the wrong shape: the `deep-research` workflow already does
> decomposition + parallel fan-out + verification + synthesis, and its ephemeral
> subagents avoid the per-agent sandbox/entitlement cost that blocked the fleet. New
> design (spec §4.4): **Mode 1** = one-shot public research, no agent, just invoke
> `deep-research` (ships today, no gap). **Mode 2** = ONE persistent `aura` agent for
> login-state / scheduling / memory, calling `deep-research` for volume and using its
> browser tier only for login-gated / heavy-JS pages. **Task 5 (fleet) is REMOVED;
> worker clones `aura-w1/-w2` are deleted.** Tasks 1–4 stand (single `aura` + browser
> tier + skills). Task 6 is reframed to Mode-1/Mode-2 verification.

**Goal:** Deliver a two-mode research capability: Mode 1 (invoke `deep-research`, no agent) and Mode 2 (one persistent `aura` agent adding login/schedule/memory), sharing one research core.

**Architecture:** Composed from existing MUR primitives, not new runtime code. Mode 1 needs no build (the `deep-research` workflow exists). Mode 2 = a single `mur agent` profile + the `agent-browser` MCP tool + five User-scoped skills. Parallelism lives in the workflow's ephemeral subagents, not a fleet.

**Tech Stack:** MUR CLI (`mur agent`, `mur skill`), `deep-research` workflow, `agent-browser` (npm, Apache-2.0), Lightpanda engine (AGPL-3.0, subprocess-only).

## Global Constraints

- Brand name user-facing is uppercase **AURA**; internal `name` is lowercase `aura` (matches on-disk dir + runtime spoof check). — spec §4.1, CLAUDE.md rule 7.
- Lightpanda (AGPL-3.0) may only be invoked as a **separate subprocess over CDP** — never linked in-process, never forked/modified. Ship unmodified upstream binary + AGPL attribution. — spec §3.
- `agent-browser` is the single control surface; engine/args/executable are set via env (`AGENT_BROWSER_ENGINE`, `AGENT_BROWSER_ARGS`, `AGENT_BROWSER_EXECUTABLE_PATH`) or per-call flags, fleet logic unchanged. `--engine lightpanda` requires the Lightpanda binary installed separately + `AGENT_BROWSER_ARGS=""` (Chrome stealth args must not reach lightpanda). — spec §4.3, verified 0.31.1.
- Mode 2 persistence (login/schedule/memory) is the ONLY justification for the agent; if none are needed, use Mode 1 (no agent). — spec §4.4.
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
from reaching Lightpanda. IMPORTANT (verified): `mur agent mcp add` uses clap, which
rejects a `--arg` value that begins with `--`; use the `--arg=<value>` form for those.
Add `--force` to skip the non-interactive install prompt.
```bash
mur agent mcp add aura agent-browser --command agent-browser --force \
  --arg=--engine --arg=lightpanda \
  --arg=--executable-path --arg="$HOME/.mur/aura/lightpanda" \
  --arg=--args --arg="" \
  --arg=mcp --arg=--tools --arg=core
mur agent mcp list aura
```
Expected: `agent-browser` listed as `agent-browser --engine lightpanda --executable-path <path> --args  mcp --tools core` (the empty `--args` shows as a double space; confirm the stored `profile.yaml` args array has a discrete `''` element). Command resolved on PATH and pinned.

- [ ] **Step 2: Add the chrome browser tool (fallback for anti-bot/screenshots)**

The MCP tools don't expose per-call engine switching, so the chrome fallback is a
second server. It inherits the global stealth args (no `--args ""`).
```bash
mur agent mcp add aura agent-browser-chrome --command agent-browser --force \
  --arg=--engine --arg=chrome \
  --arg=mcp --arg=--tools --arg=core
mur agent mcp list aura
```
Expected: both `agent-browser` (lightpanda) and `agent-browser-chrome` listed. Both invoke the `agent-browser` binary, so the spawn allowlist has one entry `["agent-browser"]` covering both. The `aura-research-escalation-ladder` skill (Task 4) tells the agent to prefer the lightpanda tool and fall back to chrome only for anti-bot/screenshot pages.

- [ ] **Step 3: Verify both tools are visible to the agent**

```bash
mur agent mcp list aura      # both servers listed
mur agent mcp inspect aura   # doctor-like: both should report status CLEAN, exit 0
mur agent status aura        # profile loads, Active: stopped
mur agent card aura          # valid A2A card
```
Expected: both MCP servers `CLEAN`; no spawn-allowlist errors. NOTE: `mur agent doctor aura` does NOT exist (doctor takes no agent arg) — use `mcp inspect` / `status` / `card`.

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
mur skill new aura-browser-preflight --dir ~/.mur/skills
mur skill new aura-research-escalation-ladder --dir ~/.mur/skills
mur skill new aura-source-triangulation --dir ~/.mur/skills
mur skill new aura-citation-discipline --dir ~/.mur/skills
mur skill new aura-parallel-fanout --dir ~/.mur/skills
```
Expected: five `skill.yaml` files scaffolded under `~/.mur/skills/`. NOTE (verified): `mur skill new` scaffolds into the CURRENT directory unless you pass `--dir ~/.mur/skills` — without it you pollute the repo root. `mur skill scope <name> --fleet <FLEET>` requires the fleet NAME (`aura-research`), not a bare `--fleet` flag; the fleet need not exist yet (name is only slug-validated).

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
mur skill scope aura-browser-preflight --fleet aura-research
mur skill scope aura-research-escalation-ladder --fleet aura-research
mur skill scope aura-source-triangulation --fleet aura-research
mur skill scope aura-citation-discipline --fleet aura-research
mur skill scope aura-parallel-fanout --fleet aura-research
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

### Task 5: ~~Create the `aura-research` fleet~~ — REMOVED

The fleet-of-workers shape is dropped (see revision banner + spec §4.4). Parallelism is
provided by the `deep-research` workflow's ephemeral subagents, not persistent agents.
The worker clones `aura-w1/-w2` and the `aura-research` fleet created during the build
were deleted. Nothing to do here.

### Task 6: End-to-end acceptance — a real research question

Prove the whole system produces a cited, verified report and that the escalation ladder and kill-switch behave.

**Files:**
- Create: `docs/superpowers/plans/artifacts/aura-e2e-report.md` (captured output)

**Interfaces:**
- Consumes: Mode 1 = the `deep-research` workflow (exists); Mode 2 = Tasks 1–4.
- Produces: acceptance evidence for both modes.

- [ ] **Step 1: Mode 1 — one-shot public research (ships today, no gap)**

Invoke the `deep-research` workflow directly with the research question (no agent, no
fleet). This is the common path and does not touch the per-agent sandbox/entitlement.
Expected: a synthesized report where claims carry source URLs + quotes. Capture to the
artifact file. (Verified pattern: the two deep-research runs during this build produced
exactly this — cited, adversarially-verified reports.)

- [ ] **Step 2: Verify citation discipline held**

Inspect the Mode-1 report: every non-trivial claim has a URL + quote; no unsourced
claims. Deep-research already enforces this (verify phase + `sources`); confirm.
Expected: PASS = zero unsourced claims.

- [ ] **Step 3: Mode 2 — persistent agent login-gated fetch (the agent's reason to exist)**

Interactively drive `aura` at a page a workflow can't reach (login-gated or heavy-JS):
```bash
mur agent cli aura
# prompt: "Use the browser tool to open <a JS/login page> and extract <X>."
```
Expected: `aura` uses `agent-browser` (lightpanda→chrome per the escalation-ladder
skill) and returns the content. This is the ONLY path that touches the per-agent
network entitlement — and is gated on the operator's `network.outbound` decision (see
Runtime gaps §3). Requires `aura` running (`mur agent install-service aura`) with the
PATH + sbpl entitlements from Runtime gaps §1–2 applied.

- [ ] **Step 4: Confirm scheduling/memory hooks (Mode 2 differentiators)**

Verify the persistence features that justify Mode 2 over Mode 1:
```bash
mur agent schedule --help    # confirm aura can be scheduled (nightly/triggered research)
```
Expected: scheduling is available; auth-vault + memory noted as follow-on (§ open questions).

- [ ] **Step 5: Commit acceptance evidence**

```bash
git add docs/superpowers/plans/artifacts/aura-e2e-report.md
git commit -m "test(aura): acceptance — Mode 1 cited report + Mode 2 login-gated fetch"
```

---

## Runtime integration gaps (discovered during the 2026-07-08 build)

Task 6's live fleet run proved the orchestration layer (router plans a DAG, splits
work across workers) but exposed real MUR runtime gaps that block autonomous browser
research under the enforcing macOS sbpl sandbox. Executors must budget for these:

1. **`install-service` gives the runtime no PATH.** The generated launchd plist has no
   `EnvironmentVariables.PATH`, so the agent runtime gets `/usr/bin:/bin:...` and can't
   find a PATH-installed MCP binary (`agent-browser` in npm's global bin). Do NOT fix
   by putting an absolute path in the profile (breaks portability/export). Fix in the
   environment: patch the plist `EnvironmentVariables.PATH` (derive npm bin from
   `npm config get prefix`), then `launchctl unload/load`. Real fix = install-service
   should propagate the user's PATH.
2. **sbpl sandbox blocks the browser toolchain by default.** Even with the binary
   found, the kernel sandbox denies exec (`Operation not permitted`). Grant (low-risk):
   `mur agent perm allow-read <a> <npm-module-dir>`, `allow-read` the lightpanda binary
   and `~/.agent-browser`, `allow-write <a> ~/.agent-browser`, `allow-spawn <a>
   ~/.mur/aura/lightpanda`.
3. **BLOCKER — no managed network mode for research.** `entitlements.network.outbound`
   is `restricted` + a STATIC host allowlist (can't express the open web) or
   `unrestricted` (no control; auto-mode classifier blocks flipping it). A web
   researcher hits arbitrary result domains, so it can't function under a static
   allowlist and shouldn't be silently made unrestricted. This is a genuine MUR product
   gap — there is no *restricted-but-managed* egress mode (audited/logged broad egress,
   per-MCP-tool scoping, or a revocable time-boxed grant). See
   `mem:gap_agent_network_entitlement_no_managed_research_mode`. Until MUR adds one,
   fully autonomous fleet web-research requires an explicit operator decision to set
   `network.outbound unrestricted` — do not do this without per-run consent.

**Net status:** Tasks 1–5 are complete and shippable; the browser tier is verified at
the CLI level (lightpanda renders; the MCP server exposes 29 tools). The autonomous
fleet run (Task 6) is gated on gap #3, which is a MUR limitation, not an AURA defect.

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
