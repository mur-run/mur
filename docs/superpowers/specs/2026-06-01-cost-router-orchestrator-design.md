# Cost-Router Orchestrator — Design Spec

> **Date**: 2026-06-01
> **Status**: Ready for review (Phase 0 design capture; decisions locked via Q&A 2026-05-31 → 2026-06-01)
> **Scope**: Agent-team foundation — a governed, local-first orchestrator that routes the easy majority of sub-tasks to cheap/local models and spawns a frontier coding agent only for the hard parts. Phase 0 (this doc) + Phase 1 (Router on the model registry). Governed-spawn (Phase 2) is **gated on P0b** and deferred.

## Overview

The Cost-Router Orchestrator is **not** a codegen competitor to Cursor / Claude Code. Its value is **optimal token/$ use**: run the easy ~80% of sub-tasks on free/local models, and **spawn a frontier coding agent (`claude` / `codex` / `agy`) only for the hard parts**. For coding, MUR is the **conductor** that spawns the frontier agent; it does not write the code itself.

This reframes a "dev team" as **on-brand** — as a *governed cost-router*, not a codegen competitor. It reverses the earlier "avoid a dev team" verdict in `2026-05-31-agent-action-pipeline-design.md`'s roadmap review.

### Why this is defensible

"Route to a cheap model" *alone* is commoditizing — pi, OpenRouter, LiteLLM, Cline, RooCode and the Claude Code model-picker all do it. The moat is the combination on top:

**Automatic × Governed × Team × Memory-flywheel.** A local-first orchestrator (≈0 marginal cost) that AUTO-decides cheap-vs-frontier per sub-task, spawns frontier CLIs as **sandboxed (B1) + signed + Commander-audited** sub-processes, coordinates a role-team, and **gets cheaper over time** as accumulating memory raises the local model's hit-rate (escalation frequency ↓ ⇒ cost ↓). We do **not** pitch "we route to cheap models" — that is table-stakes plumbing.

### Research Foundation

Design validated against 2026 ecosystem practice and a focused gating investigation:

- **Manual routing is the current norm, automatic+governed is the gap**: pi (`@mariozechner/pi-coding-agent`) popularized 15+ providers, mid-session model switch, and cheap/local stacks — but its routing is **manual** (`/model`), it has **no native sub-agents** (DIY via tmux), and its bash tool is **ungoverned**. OpenRouter / LiteLLM provide the routing *plumbing* but no governance, team, or memory layer.
- **Frontier coding agents are driven as governed subprocesses or via vendor SDK**: the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview) (formerly Claude Code SDK) exposes "the same tools, agent loop, and context management that power Claude Code," programmable in Python/TS — i.e. you **reuse** the vendor's native agent loop instead of reimplementing it.
- **Claude has NO native A2A** (gating finding, see below): Claude's native interop protocol is **MCP**; A2A support exists only via non-production community wrappers ([ericabouaf/claude-a2a](https://github.com/ericabouaf/claude-a2a) — README warns "not production ready", [caomyer/claude-code-a2a-multiagent](https://github.com/caomyer/claude-code-a2a-multiagent)). The Claude Agent SDK's multi-agent story is **sub-agents + structured-artifact hand-off** (Planner/Generator/Evaluator), not A2A; native A2A lives in Google ADK / CrewAI ([Composio 2026 comparison](https://composio.dev/content/claude-agents-sdk-vs-openai-agents-sdk-vs-google-adk)). Anthropic engages A2A only in partner contexts ([Vertex AI MCP+A2A webinar](https://www.anthropic.com/webinars/deploying-multi-agent-systems-using-mcp-and-a2a-with-claude-on-vertex-ai)).
- **Adapter churn is real — adapters must be disposable**: Google is sunsetting **Gemini CLI for free/individual users on 2026-06-18**, replacing it with the Go-based, closed-source **Antigravity CLI (`agy`)** ([Google Developers Blog](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/), [The Register](https://www.theregister.com/ai-ml/2026/05/20/bye-bye-gemini-cli-google-nudges-devs-toward-antigravity/5243605), [Google Cloud comparison](https://cloud.google.com/blog/topics/developers-practitioners/choosing-antigravity-or-gemini-cli)). `agy` supports headless invocation (`agy -p "…" --output-format json`, async sub-agents — [DataCamp guide](https://www.datacamp.com/tutorial/antigravity-cli)) but ships with **tight free-tier quotas**. A major vendor killed its CLI on ~30-day notice: the orchestrator must treat concrete adapters as replaceable.

## Locked Decisions

Captured via Q&A (2026-05-31 → 2026-06-01). These are the inputs this spec encodes:

| # | Decision | Choice |
|---|----------|--------|
| 1 | **Routing autonomy** | **Hybrid** — default auto by a difficulty heuristic, with per-role / per-task override. Implemented in Phase 1. |
| 2 | **Spawn mechanism** | **Governed subprocess** abstraction (option A, refined toward vendor SDK). A2A-to-drive-Claude (option C) **ruled out** by gating check. Own-API-loop (option B) rejected — reimplements the vendor agent loop. |
| 3 | **Scope this round** | **Phase 0 + Phase 1** only (this doc + Router on the model registry). Governed-spawn (Phase 2) is gated on the unbuilt **P0b agentic loop** and deferred. |
| 4 | **Flagship team / GTM** | **Dev team** first (conductor that spawns `claude`/`codex`/`agy`) · **consumer companion** GTM lane (aligns with `2026-05-29-mur-strategy-positioning-vs-archon.md`). |
| 5 | **Landing** | Own design spec + PR (this doc), **not** folded into the action-pipeline workstream. |
| 6 | **v1 adapter set** | Ship **all three** reference adapters (`claude` / `codex` / `agy`); `agy` documented **best-effort / provider-in-flux**. Gemini-family targets **`agy`**, not legacy `gemini-cli`. |

## Architecture

### Overview

```
┌───────────────────────────────────────────────────────────────────────┐
│                     Cost-Router Orchestrator                           │
│                                                                        │
│   Sub-task ──▶ ┌──────────────┐  easy (~80%)  ┌────────────────────┐   │
│                │   Router      │ ────────────▶ │ cheap / local model │   │
│                │ (hybrid)      │               │  (≈0 marginal cost) │   │
│                │  • difficulty │               └────────────────────┘   │
│                │    heuristic  │  hard (~20%)  ┌────────────────────┐   │
│                │  • per-role   │ ────────────▶ │  Governed Spawn     │   │
│                │    override   │   escalate    │  (CodingAgentAdapter)│  │
│                └──────────────┘               └─────────┬──────────┘   │
│                        ▲                                 │              │
│                        │ hit-rate ↑ over time            ▼              │
│                ┌───────┴────────┐            ┌──────────────────────┐   │
│                │  Memory flywheel │◀───────── │  B1 sandbox wrapper   │   │
│                │ (patterns/skills)│  inject    │ Landlock/seccomp/SBPL │   │
│                └──────────────────┘  via MCP   │ + signed profile      │   │
│                                                └─────────┬────────────┘   │
│                                                          ▼                │
│   Frontier subprocess: claude (Agent SDK) │ codex exec │ agy -p           │
│                                                          │                │
│   ┌──────────────────────────────────────────────────────────────────┐  │
│   │      Audit ledger (JSONL) — Commander-visible, per-spawn          │  │
│   │   ~/.mur/actions/ledger/YYYY-MM-DD.jsonl  (cmd, tokens, $, verdict)│  │
│   └──────────────────────────────────────────────────────────────────┘  │
└───────────────────────────────────────────────────────────────────────┘
```

### Design Decision: spawn mechanism

Three mechanisms were evaluated for invoking the frontier coding agent. A focused gating investigation (deep-research, halted early per the "stop if Claude lacks A2A" instruction) resolved this without a full study:

| Mechanism | Governance / sandbox | Multi-turn control | Tool-use fidelity | Cost / token visibility | Maintenance | Lock-in | Verdict |
|-----------|----------------------|--------------------|-------------------|-------------------------|-------------|---------|---------|
| **(A) Vendor CLI / SDK as governed subprocess** | OS-level via B1 (process is the boundary) | stdio / cancel / context files | **Native loop preserved** | parse `--output-format json` / SDK usage events | adapter per CLI; **pin version + probe** | low (swap adapter) | ✅ **Chosen** |
| (B) Own provider API + own agentic loop | full (we own the loop) | full | **reimplemented** (quality risk) | native (we count tokens) | high — own loop + per-provider tool schemas; **needs P0b** | medium | ❌ reimplements vendor loop |
| (C) A2A / ACP-bound sub-agents | full at protocol layer | protocol messages | depends on bridge | bridge-dependent | **blocked — Claude has no native A2A** | low | ❌ ruled out for driving Claude |

**Conclusion**: drive each frontier agent via **(A) the vendor CLI / SDK as a B1-sandboxed subprocess** — reuse the native agent loop (keep tool-use fidelity and reasoning quality), capture `stdout`/JSON for audit + token/$ visibility, and enforce file/network scope at the OS boundary regardless of whether the vendor binary is open or closed source. **MCP** is the channel to inject MUR memory into a spawned `claude` (Claude is MCP-native) — the salvageable part of option C. **A2A is reserved for MUR↔MUR internal agent coordination** (where MUR controls both ends and already speaks A2A v0.3 via `mur-agent-runtime`), not for driving third-party agents.

### `CodingAgentAdapter` contract

The stable contract; concrete adapters are **disposable** (see Roadmap Alignment — adapter churn).

```rust
// mur-core/src/route/adapter.rs (illustrative)

/// One frontier coding agent, driven as a governed subprocess.
/// Concrete impls are pinned to a CLI version + capability-probed at runtime.
trait CodingAgentAdapter {
    /// Stable identifier, e.g. "claude", "codex", "agy".
    fn id(&self) -> &str;

    /// Runtime probe — flags / output formats differ and churn across versions.
    /// Probing (not hardcoding) is mandatory: a vendor changed its CLI on ~30-day notice.
    fn probe(&self) -> Result<AdapterCapabilities>;

    /// Spawn one governed turn. The supervisor wraps the child in a B1 sandbox
    /// profile and a signed agent profile before exec.
    fn spawn_turn(&self, req: SpawnRequest, sandbox: &SandboxProfile)
        -> Result<SpawnHandle>;
}

struct SpawnRequest {
    prompt: String,
    /// Memory injected to raise the local hit-rate / brief the frontier agent.
    /// claude → MCP server endpoint; others → context files / native skills.
    memory: MemoryInjection,
    cwd: PathBuf,
    deadline: Option<Duration>,
}

/// Normalised event stream + usage, parsed from the vendor's structured output.
struct SpawnHandle {
    events: EventStream,          // tool calls, text, status
    usage: UsageMeter,            // tokens in/out + $ estimate → audit ledger
    cancel: CancelToken,          // mid-task interjection / cancellation
}
```

### Reference adapters (v1)

All three ship in v1 (decision #6). Invocation strings are **illustrative** — resolved by `probe()` at runtime, never hardcoded.

| Adapter | Drive via | Headless invocation (probed) | Memory injection | Status |
|---------|-----------|------------------------------|------------------|--------|
| `claude` | Claude Agent SDK / `claude -p` | `claude -p … --output-format stream-json` | **MCP** (native) — MUR exposes pattern/skill memory as an MCP server | stable, reference |
| `codex` | OpenAI Codex CLI | `codex exec … ` (structured output) | context files / system prompt | stable, reference |
| `agy` | Antigravity CLI | `agy -p "…" --output-format json` | Antigravity plugins / context | **best-effort / provider-in-flux** (closed-source, tight free quotas; replaces sunset `gemini-cli`) |

### Governance

Every spawned frontier subprocess passes the **same gate as any single MUR agent**:

- **B1 sandbox** (`mur-agent-runtime`): Landlock v4 + seccomp + SBPL + Job Object + HostGuard / birdcage — file & network scope enforced at the OS boundary, so a **closed-source** binary (`agy`) is governed identically to an open one.
- **Signed agent profile** — the spawn carries a signed profile; unsigned/oversized capability grants are rejected.
- **Audit ledger** — one JSONL record per spawn under `~/.mur/actions/ledger/YYYY-MM-DD.jsonl`: command, model, tokens in/out, $ estimate, exit verdict. This is the cost/$ visibility surface and the Commander audit trail.

### Memory flywheel

The cost curve bends down over time: accumulated patterns/skills are injected (via **MCP** for `claude`, via context/native-skills for others) to (a) brief the frontier agent and (b) raise the **local** model's hit-rate so fewer sub-tasks escalate. **Escalation frequency is the cost metric** — the audit ledger's escalation rate is the KPI this system optimizes.

### Routing (Phase 1)

Built on the existing model registry (`~/.mur/models.yaml`, `mur model …`). **Hybrid** (decision #1):

- **Default auto** — a difficulty heuristic scores each sub-task (task type, estimated context size, prior escalation/failure history for similar tasks, role config) and picks cheap-local vs frontier.
- **Override** — per-role config (pi-style frontmatter) or a per-task flag forces a specific tier/model.
- Escalation events and outcomes feed back into the heuristic and the audit ledger.

## Roadmap Alignment

This spec is the **agent-team foundation** and is bounded by MUR's stated moat — local-first + ≈0 marginal cost + accumulating memory + cryptographic governance (`docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md`). Constraints carried over from the action-pipeline red-team (`2026-05-31-agent-action-pipeline-design.md`):

- **Governed cost-router reverses "avoid a dev team."** The dev team is on-brand *as a conductor*, not a codegen competitor.
- **Flagship ships narrow, not broad** — one deep **dev team** first, not a wide catalog of thin agents.
- **Agent Teams are first-party, curated, signed bundles** gated to a paid tier — not an open third-party marketplace. A spawned frontier agent is a large capability grant and passes the same gate + sandbox + audit as any single agent.
- **Adapters are disposable.** Concrete CLI adapters are pinned + runtime-probed and may be deprecated/replaced without touching the `CodingAgentAdapter` contract (Gemini CLI → `agy` is the worked example).
- **No visual DAG/flow editor** (carried from the action-pipeline roadmap).

## Scope & Phasing

| Phase | Deliverable | This round? | Gate |
|-------|-------------|-------------|------|
| **0** | This design spec | ✅ | — |
| **1** | Router on the model registry (hybrid auto + override); audit-ledger escalation metric | ✅ | — |
| **2** | Governed spawn-tool (`CodingAgentAdapter` + 3 reference adapters + B1 wrap) | ⛔ deferred | **P0b agentic loop** (not yet built) |
| **3** | Team manifest + flagship **dev team** | ⛔ deferred | Phase 2 |

**Explicitly deferred**: governed-spawn implementation, the team manifest, and any flagship-team UX. Phase 1 lands the Router and the cost-visibility ledger so the savings thesis is measurable *before* spawn is built.

## Risks & Open Questions

- **`agy` as a free-tier escalation target is weak** — closed-source + tight free quotas. For the **consumer companion** lane, `claude` + `codex` are the dependable escalation pair; `agy` ships best-effort. Re-evaluate when `agy`'s quota / OSS situation settles.
- **P0b dependency** — Phase 2 cannot start until the agentic loop exists. Phase 1 is designed to deliver standalone value (measurable routing + cost) without it.
- **Provider lock-in** — mitigated by the adapter contract; the orchestrator core never imports a vendor SDK directly.
- **Open**: do `codex exec` and `agy -p` expose token/usage in structured output sufficient for the ledger, or is estimation needed? Resolve via `probe()` during Phase 2.

## References

Anthropic / Claude: [Agent SDK overview](https://code.claude.com/docs/en/agent-sdk/overview) · [MCP+A2A on Vertex AI (webinar)](https://www.anthropic.com/webinars/deploying-multi-agent-systems-using-mcp-and-a2a-with-claude-on-vertex-ai) · A2A wrappers (non-production): [ericabouaf/claude-a2a](https://github.com/ericabouaf/claude-a2a), [caomyer/claude-code-a2a-multiagent](https://github.com/caomyer/claude-code-a2a-multiagent). Framework landscape: [Composio 2026 comparison](https://composio.dev/content/claude-agents-sdk-vs-openai-agents-sdk-vs-google-adk). Gemini CLI sunset → Antigravity CLI: [Google Developers Blog](https://developers.googleblog.com/an-important-update-transitioning-gemini-cli-to-antigravity-cli/) · [Google Cloud comparison](https://cloud.google.com/blog/topics/developers-practitioners/choosing-antigravity-or-gemini-cli) · [The Register](https://www.theregister.com/ai-ml/2026/05/20/bye-bye-gemini-cli-google-nudges-devs-toward-antigravity/5243605) · [DataCamp `agy` guide](https://www.datacamp.com/tutorial/antigravity-cli). Related internal: `docs/superpowers/specs/2026-05-29-mur-strategy-positioning-vs-archon.md`, `docs/superpowers/specs/2026-05-31-agent-action-pipeline-design.md`.
