# MuR Unified Skill Ecosystem — Design Spec

**Date**: 2026-05-24
**Status**: Draft
**Scope**: Unified skill authoring, storage, runtime injection, composition, registry, peer transfer, agent-generated skills, security & supply chain risk management, lifecycle management, automated evolution, MCP integration, and observability

## 1. Motivation

MuR currently has two separate "skills" concepts that share no code, format, or runtime behavior:

| | Built-in AI Tool Skills | Agent Skills |
|---|---|---|
| **Location** | `~/.mur/skills/<name>/SKILL.md` | `<agent_home>/skills/<name>.md` |
| **Purpose** | Teach AI coding assistants how to use mur | Extend mur agent capabilities |
| **Format** | Markdown with YAML frontmatter | Plain markdown |
| **Runtime injection** | Via hook system at session start | **Not injected** (advertised in Agent Card only) |
| **Composition** | None | None |
| **Discovery** | `mur sync` symlinks | Agent Card only |

This design unifies both into a single structured skill ecosystem supporting agent-to-agent skill transfer (peer learning + registry marketplace), human+agent co-authorship, composable skills, and a defense-in-depth security model drawn from mur's existing B0/B1 runtime enforcement and mur-commander's constitution + trust system.

### 1.5 Coordination Boundary

A skill describes **what to do**, never **how the host should recover when it fails**. Cross-step coordination — retry policy, microstep journaling, replay-on-restart, deterministic ordering — is the **host's** responsibility, not the skill's.

This boundary is intentional. Skills are portable across hosts (mur agent runtime, mur-commander, future hosts) precisely because they do not encode host-specific recovery logic. Each host implements a coordination protocol that meets a shared contract.

The contract requires every host to:

1. **Journal** each skill load and each procedure step with a stable `skill_id@version` reference.
2. **Microstep** skill procedure steps within the host's larger plan, so they appear in the host's coordination journal alongside non-skill steps.
3. **Apply the host's failure-recovery policy** (retry / reroute / escalate) to skill step failures, classified by the `FailureCategory` taxonomy (Knowledge / Tool / Clarification / Style / Transient — same taxonomy as §8.2).
4. **Resume on restart** using the host's journal — skills carry no replay state.

Skills that need behaviour beyond what the host coordination protocol guarantees (e.g. a skill that must run inside a transaction) MUST declare this in `capabilities_declared` and a host that cannot meet the requirement MUST refuse to load the skill.

## 2. Security & Supply Chain Risk Management

**This is M0 priority.** The threat landscape is severe and active as of 2026: 36.8% of public skills contain security flaws, 1,184 malicious skills were found in one marketplace (247K+ installations), and 82% of public MCP servers lack path-traversal protections. The average malicious skill combines 4 attack vectors: traditional malware + prompt injection + credential exfiltration + memory poisoning. A skill ecosystem without security at its foundation is a supply chain attack vector, not a feature.

### 2.1 Threat Model

Skills face five attack surfaces adapted from OWASP AST10 (Agentic Skills Top 10, April 2026):

| ID | Risk | Severity | Mitigation |
|----|------|----------|------------|
| AST01 | Malicious Skills (DDIPE, prompt injection in skill body) | Critical | Content scanning + sandboxed execution |
| AST02 | Supply Chain Compromise (typosquatting, dependency hijacking) | Critical | Registry verification + binary pinning |
| AST03 | Over-Privileged Skills (no capability declaration) | High | Entitlement declaration + enforcement |
| AST04 | Memory Poisoning (skill rewrites MEMORY.md / system prompt) | High | Immutable instruction zones + drift detection |
| AST05 | Exfiltration (skill reads secrets and sends to C2) | Critical | Secret pre-filter + outbound network gating |
| AST06 | Ranking Manipulation (fake downloads push malicious skills to #1) | Medium | Registry auth + download attestation |
| AST07 | Unicode/Bidi Obfuscation (evades signature scanning) | Medium | NFC normalization + glyph-aware scanning |
| AST08 | Time-Gated Payloads (malicious behavior activates after N days) | High | Continuous behavioral monitoring |
| AST09 | Cross-Platform Reuse (skill safe on Linux, dangerous on macOS) | Medium | Platform capability matrix |
| AST10 | Tool Description Poisoning (MCP tool descriptions are the attack) | High | Tool description allowlist + capability gating |

### 2.2 Defense-in-Depth Architecture

MuR already has substantial security infrastructure across two codebases. The skill security model layers them:

```
Layer 1: Content Integrity (static analysis)
  ├─ mur-common: executable content ban (muragent/executable_ban.rs)
  ├─ mur-common: DSSE + Ed25519 package signing (muragent/dsse.rs)
  ├─ mur-common: in-toto subject hash verification (muragent/validator.rs)
  ├─ NEW: skill content scanner (prompt injection patterns, secret patterns)
  └─ NEW: Unicode NFC normalization + bidi detection (from mur-commander constitution signing.rs)

Layer 2: Identity & Trust (who published this?)
  ├─ mur-common: Ed25519 AgentIdentity (identity.rs) — reuse for skill signing
  ├─ mur-core: Ed25519 character card signing (character_card/signing.rs)
  ├─ mur-commander: three-tier TrustStore (Sandboxed < Verified < Trusted)
  │   └─ engine/src/trust/ — JSON-backed, 0o600 perms, atomic writes, constant-time checksum
  └─ NEW: SkillTrustStore — per-skill trust level derived from publisher + verification

Layer 3: Runtime Sandboxing (what can the skill do?)
  ├─ mur-agent-runtime: Landlock V4 + seccomp BPF (sandbox/linux.rs)
  ├─ mur-agent-runtime: macOS SBPL (sandbox/macos.rs) — deny-by-default Seatbelt profiles
  ├─ mur-agent-runtime: Windows Job Object (sandbox/windows.rs)
  ├─ mur-agent-runtime: reqwest HostGuard DNS-level network gating (sandbox/reqwest_guard.rs)
  ├─ mur-agent-runtime: birdcage child process sandbox (sandbox/child.rs)
  ├─ mur-commander: OS sandbox (sandbox/os.rs) + resource limiter (sandbox/limiter.rs)
  └─ NEW: SkillSandboxPolicy — per-trust-level resource limits applied at skill load time

Layer 4: Runtime Hook Enforcement (what rules apply at execution time?)
  ├─ mur-agent-runtime: B0SafetyHook — 8+ rules (hooks/b0.rs)
  │   ├─ Rule 1: FS confinement (path_confined_to)
  │   ├─ Rule 2: Outbound network gating (host_is_allowlisted + GrantStore)
  │   ├─ Rule 4: Side-effect gating after untrusted input
  │   ├─ Rule 5: Process spawn gating (deny/allowlist/any)
  │   ├─ Rule 6: MCP binary SHA-256 pin verification
  │   ├─ Rule 7: Credential pre-filter (11 secret patterns in b0_helpers.rs)
  │   ├─ Rule 8: PII redaction on memory.* tool outputs
  │   └─ Rule 11: Native binary code signing check (codesign/signtool)
  ├─ mur-commander: Constitution system (constitution/) — Ed25519-signed tamper-proof rules
  │   └─ engine/src/constitution/signing.rs — constant-time SHA-256 + Ed25519 verification
  ├─ mur-commander: PolicyEngine (policy/engine.rs) — rules can only downgrade, never upgrade
  ├─ mur-commander: PathGuard (gateway/src/auth/path_guard.rs) — per-user filesystem ACL
  └─ NEW: SkillConstitution — per-skill or per-trust-level safety rules
```

### 2.3 Skill Trust Model

Adapted from mur-commander's three-tier `TrustLevel` (`engine/src/trust/level.rs`):

| Trust Level | Source | Default Capabilities |
|-------------|--------|---------------------|
| **Sandboxed** (default) | Peer transfer, agent-generated, untrusted registry | Read-only within agent_home; no network; no spawn; strict timeout; token budget cap |
| **Verified** | Registry-verified (checksum match), community-reviewed | Read/write within agent_home; restricted outbound network; allowlisted spawn; normal limits |
| **Trusted** | First-party (built-in), user-approved, signed by trusted publisher | Full agent capabilities per entitlements; standard limits |

Trust transitions:
```
Sandboxed → Verified: user approval + checksum verification + N days without incident
Verified → Trusted: user explicit promotion + publisher signature verification
Any → Revoked: incident detected → auto-revoke + audit log
```

### 2.4 Skill-Specific Security Requirements

**At install time** (static):
1. Content scan: detect known prompt injection patterns (DDIPE), secret patterns, code-execution markers
2. Unicode normalization: NFC + bidi character detection (mur-commander pattern from `constitution/signing.rs` lines 217-237, SEC-14)
3. Executable content ban: extend mur's existing `executable_ban.rs` to skill body (no embedded shell/python/js)
4. Dependency audit: recursive `requires` scan against known-vulnerable skill versions
5. Publisher verification: if signed, verify Ed25519 signature (reuse `identity.rs` + `character_card/signing.rs`)

**At load time** (runtime):
1. Capability declaration enforcement: skill declares required capabilities; denied if exceeds trust level
2. Instruction boundary: skill body is wrapped in `<skill-instruction source="..." trust="...">` tags, separated from system prompt by mur-commander's `<untrusted_tool_result>` pattern
3. Resource limits applied: timeout, output size, call count — from mur-commander `sandbox/limiter.rs`

**At execution time** (continuous):
1. B0 hook chain: all existing B0 rules apply to skill-triggered tool calls
2. Drift detection: skill file SHA-256 compared to install-time pin (reuse `b0_helpers.rs` MCP pin pattern)
3. Memory poisoning guard: skill modification to `profile.yaml` or `sys_prompt.md` blocked unless via `mur agent skill` CLI

### 2.5 Reuse of Existing Security Infrastructure

| Existing mur/mur-commander component | Reused for skill security |
|--------------------------------------|--------------------------|
| `mur-common/src/muragent/executable_ban.rs` | Skill body executable content scan |
| `mur-common/src/muragent/dsse.rs` (DSSE + Ed25519) | Skill package signing |
| `mur-common/src/muragent/validator.rs` (11-step) | Skill package validation |
| `mur-common/src/identity.rs` (Ed25519 keys) | Skill publisher identity |
| `mur-agent-runtime/src/sandbox/` (Landlock/SBPL/Job) | Per-skill sandboxing |
| `mur-agent-runtime/src/hooks/b0.rs` (8 rules) | Skill execution enforcement |
| `mur-agent-runtime/src/hooks/b0_helpers.rs` (11 secret patterns) | Skill body secret scan |
| `mur-commander crates/engine/src/trust/` (TrustStore, 3-tier) | Skill trust model |
| `mur-commander crates/engine/src/constitution/signing.rs` (SEC-14/15) | Unicode + constant-time verify |
| `mur-commander crates/gateway/src/auth/path_guard.rs` | Per-skill filesystem ACL |
| `mur-commander crates/engine/src/policy/engine.rs` | Skill policy enforcement |
| `mur-common/src/permissions.rs` (GrantStore + audit) | Skill permission grants |

### 2.5.1 Concurrent-Host Safety

Multiple hosts (mur agent runtime + mur-commander + future hosts) may concurrently write to `~/.mur/trust/skills.json`. The trust store uses:

1. **File locking** via `fs2::FileExt::lock_exclusive()` before read-modify-write.
2. **Atomic rename** for writes (`write to .tmp`, `fsync`, `rename`).
3. **Per-skill-per-host entries**: `approved_by_host: Vec<HostId>` so different hosts can hold different opinions about a skill's trust level. Trust is per (skill, host); not global.

Reuse the atomic-write + constant-time-checksum primitives from mur-commander `engine/src/trust/store.rs` to avoid implementing this twice.

## 3. Skill Data Model

A skill is a self-contained unit of transferable knowledge with three progressive-disclosure layers.

### 2.1 Three-Layer Progressive Disclosure

```
+---------------------------------------------+
|  Layer 1: Manifest (always visible)          |  ~50 tokens
|  name, version, publisher, triggers, tags    |
+---------------------------------------------+
|  Layer 2: Abstract (injected at boot)        |  ~200 tokens
|  what it does, when to use, dependencies     |
+---------------------------------------------+
|  Layer 3: Body (loaded on trigger match)     |  ~2000 tokens
|  full procedure, variables, examples, errors |
+---------------------------------------------+
```

- **Layer 1** — Always visible. Broadcast in A2A Agent Cards. Registry search index.
- **Layer 2** — Injected into system prompt at session start (subject to token budget). Describes purpose, triggers, required dependencies, expected outcomes.
- **Layer 3** — Loaded on demand when the skill is triggered. Complete procedure steps, variable definitions, examples, error handling.

### 2.2 Canonical Structure

```yaml
# Canonical YAML representation
name: research-prices
version: 1.0.0
publisher: human:david
description: Search and compare product prices across e-commerce sites
category: workflow            # context | workflow | command | meta

# Which hosts may load this skill (omit = [all])
hosts: [mur-agent, mur-commander]

content:
  abstract: |
    Searches product prices on e-commerce sites.
    Triggered by `/research-prices` or keywords like "查價格", "find prices".
    Requires web-browsing skill. Outputs a sorted price table.
  procedure:
    variables:
      - name: product_name
        type: string
        required: true
        description: Product to search for
      - name: target_sites
        type: array
        required: false
        default: ["pchome", "momo", "shopee"]
        description: Sites to search
    steps:
      - description: Navigate to first target site
        tool: browser.navigate
      - description: Search for product
        tool: browser.fill
      - description: Extract price from results
        tool: browser.extract
      - description: Repeat for remaining sites
      - description: Sort results by price and present table

requires:
  - name: web-browsing
    version: ">=1.0.0"

tags: [e-commerce, price, shopping, scraping]

triggers:
  - type: command
    pattern: "/research-prices"
  - type: keyword
    pattern: "(查價格|比價|find prices|price check)"
  - type: manual

priority: normal               # low | normal | high | critical
```

### 2.3 Content Modes

Publisher identifiers use the format `human:<name>` for human-authored skills or `agent:<agent_id>` for agent-generated skills. Agent IDs match the agent's directory name in `~/.mur/agents/`.

Host compatibility is declared via `hosts: [mur-agent, mur-commander]`. The `HostId` enum lives in `mur-common::skill`:

```rust
pub enum HostId {
    MurAgent,
    MurCommander,
    All,                  // both / any future host
    Custom(String),       // reserved for future hosts
}
```

Default value (empty/missing) is `[all]` for backward compatibility. Hosts filter via `SkillLoader::filter_for_host()`.

A skill has exactly one content mode:

| Mode | Field | Description | Example |
|------|-------|-------------|---------|
| `context` | `content.context` | Declarative knowledge for system prompt | mur-context |
| `workflow` | `content.procedure` | Step-by-step with variables and tool refs | mur-run |
| `command` | `content.command` | Single triggered action | mur-in, mur-out |

`meta` category skills describe other skills (for skill discovery and composition guidance) and use `context` mode.

### 2.4 Dual Surface Format

- **Canonical YAML** — Full structured representation. What agents generate and what the runtime consumes.
- **Markdown frontmatter** — Simplified authoring format for humans. Auto-converted to canonical YAML on save.

```markdown
---
name: research-prices
version: 1.0.0
publisher: human:david
description: Search and compare product prices
category: workflow
requires:
  - web-browsing>=1.0.0
tags: [e-commerce, price]
triggers:
  - command: /research-prices
  - keyword: (查價格|比價|find prices)
---

# research-prices

Searches product prices on e-commerce sites.

## Variables
- `product_name` (string, required) — Product to search for
- `target_sites` (array, default: pchome,momo,shopee)

## Steps
1. Navigate to target site
2. Search for product_name
3. Extract price
4. Repeat for remaining sites
5. Sort and present table
```

`mur skill validate` checks both formats. `mur skill fmt` converts between them.

## 3. Storage

### 3.1 On-Disk Layout

```
~/.mur/
  skills/                          # Global skills (AI tool injection)
    research-prices/
      skill.yaml                   # Canonical source of truth
      skill.lock                   # Resolved dependency versions
    mur-context/
      skill.yaml
    ...

  agents/
    <agent>/
      profile.yaml                 # skills: ["skills/research-prices.yaml", ...]
      skills/                      # Per-agent skills (agent runtime)
        research-prices.yaml       # Same format as global skills
        ...
```

Global skills are available to all AI tool sessions. Per-agent skills are scoped to a single agent. Both use the same format.

### 3.2 Lock File

```yaml
# skill.lock — generated on install, checked into version control
locked:
  web-browsing: 1.2.0
  data-table-export: 0.6.1
installed_at: 2026-05-24T10:30:00Z
```

Ensures reproducible dependency resolution. `mur skill update` bumps locked versions.

### 3.3 Loader API

Skills are loaded via a host-implemented `SkillLoader` trait, defined in `mur-common`:

```rust
pub trait SkillLoader {
    /// Scopes this host knows about, in priority order (lowest first).
    fn scopes(&self) -> Vec<SkillScope>;

    /// Walk all scopes and return resolved manifests (later scopes override earlier).
    fn load_all(&self) -> Vec<(SkillScope, SkillManifest)>;

    /// Filter manifests to those compatible with this host id (uses §3 hosts: field).
    fn filter_for_host(&self, host: HostId) -> Vec<SkillManifest>;

    /// Verify a manifest's signature and content hash (uses §2 trust model).
    fn verify(&self, manifest: &SkillManifest) -> Result<TrustVerdict, LoadError>;
}

pub enum SkillScope {
    /// Built-in skills shipped with the host binary.
    Builtin,
    /// Global skills at `~/.mur/skills/`.
    Global,
    /// Host-private skills (e.g. `~/.mur/commander/skills/`).
    Host(PathBuf),
    /// Per-agent skills (mur agent runtime only).
    Agent { agent: String },
    /// Marketplace install at `~/.mur/marketplace/`.
    Marketplace,
}
```

- mur agent runtime provides `MurAgentSkillLoader`.
- mur-commander provides `CommanderSkillLoader`.
- The trait is the **single** integration point for hosts.

Skills cannot bypass the loader — there is no public `Skill::from_str` for arbitrary skill execution. This forces every load to pass trust verification.

## 4. Runtime Injection

### 4.1 Agent Boot Sequence

```
Agent boot
  -> Read profile.skills (per-agent) + ~/.mur/skills/ (global)
  -> Parse each skill.yaml; extract Layers 1+2
  -> Classify by trigger type:
      session_start -> inject Layer 2 into system prompt
      command/keyword -> register in trigger index
      manual -> list in Agent Card only
  -> Apply token budget
  -> Assemble system prompt
```

### 4.2 Token Budget

Skill injection is **adaptive** — the budget changes based on conversation state to prevent Context Rot (the LLM increasingly ignoring early instructions as context fills up). Commander observed measurable instruction-following degradation past ~30 turns with 4+ static skills injected.

```yaml
skills:
  # Static caps (upper bounds — always enforced)
  max_skills_in_prompt: 5
  max_total_tokens: 2000
  priority_order: [global, agent]

  # Adaptive policy (optional; omit for static-only behaviour)
  adaptive:
    # Reduce skill budget proportionally as context fills.
    # Formula: effective_budget = max_total_tokens * (1 - context_fill_ratio)^decay
    context_fill_decay: 1.5
    # Below this threshold, skip skill injection entirely.
    min_remaining_context_ratio: 0.20
    # Promote a skill's priority if it fired in the last N turns.
    recent_fire_boost_turns: 5
```

Layer 3 (body) is excluded from this budget — it loads on trigger match, replacing the skill's Layer 2 abstract in context.

Hosts MAY refuse to inject a skill when remaining context is below `min_remaining_context_ratio`, recording a `skill_skip_context_full` event in the journal. This is observable and tunable; hardcoded static budgets are not.

### 4.3 Trigger Matching

On each user prompt:
- Scan registered command triggers → match `/command` prefix
- Scan registered keyword triggers → match regex against prompt text
- On match → load skill's Layer 3 (body) into context
- No match → no action

### 4.4 Code Changes

| File | Change |
|------|--------|
| `mur-common/src/skill.rs` | New `Skill` struct + parser |
| `mur-agent-runtime/src/profile.rs` | `Profile::load()` reads skill yaml |
| `mur-agent-runtime/src/supervisor.rs` | `with_system_prompt()` injects Layer 2 |
| `mur-agent-runtime/src/task_runner.rs` | New `triggered_skill` field; trigger matching |

## 5. Composition & Dependencies

### 5.1 Dependency Declaration

Skills declare dependencies via `requires`:

```yaml
requires:
  - name: web-browsing
    version: ">=1.0.0"
  - name: data-table-export
    version: ">=0.5.0, <2.0.0"
```

### 5.2 Resolution

```
Install research-prices
  -> Check requires
  -> web-browsing installed (version 1.2.0 >= 1.0.0) -> skip
  -> data-table-export not installed -> recursive install
  -> Cycle detection: reject on circular dependencies
  -> Write skill.lock with resolved versions
```

### 5.3 Runtime Composition

Dependencies are composition, not inheritance. When `research-prices` triggers and loads Layer 3:
- Its procedure steps reference dependency skills by trigger
- The runtime loads each dependency's Layer 3 only when its trigger fires
- Dependencies are independent, testable units

### 5.4 Version Constraints

Semver matching. `>=1.0.0`, `^1.2.3`, `~1.2.3`, exact `1.2.3`. No range means `*` (any version). Lock file pins exact versions.

## 6. Registry & Discovery

### 6.1 Dual-Federated Registry

**Primary — Git-based registry** (zero-infrastructure):
```
https://github.com/mur-run/skill-registry
  index.yaml                    # Search index (auto-updated on publish)
  skills/
    research-prices/
      versions/
        1.0.0.yaml
        1.1.0.yaml
    web-browsing/
      ...
```

Publishing = opening a PR to this repo. No server required.

**Secondary — Agent Card broadcast** (decentralized):
- Each agent broadcasts skills in its A2A Agent Card (existing `skills` field)
- Other agents discover skills via `GET /.well-known/agent-card.json`
- Skill content fetched via new A2A endpoints

### 6.2 CLI

```bash
mur skill install research-prices              # From default registry
mur skill install https://github.com/...        # From git URL
mur skill install agent://my-agent              # From another agent
mur skill install ./skill.yaml                  # From local file
mur skill search "prices"                       # Search registry
mur skill search "prices" --local               # Search installed only
mur skill info research-prices                  # Layer 1+2 summary
mur skill info research-prices --full           # Complete skill
mur skill publish ./skill.yaml                  # Push to registry
mur skill update research-prices                # Update to latest
mur skill list                                  # List installed
mur skill remove research-prices                # Uninstall
mur skill validate ./skill.yaml                 # Validate format
mur skill fmt ./skill.yaml --markdown            # Convert to markdown
```

### 6.3 Search Index

`index.yaml` in registry root, regenerated on each publish:

```yaml
skills:
  research-prices:
    latest: 1.1.0
    description: Search and compare product prices
    publisher: human:david
    category: workflow
    tags: [e-commerce, price, shopping]
    downloads: 42
    rating: 4.5
```

Local cache at `~/.mur/cache/registry-index.yaml`, refreshed on `mur skill search`.

## 7. Peer Transfer Protocol

### 7.1 Pull Transfer (primary)

```
Agent A (has skill)                     Agent B (wants skill)

1. DISCOVER
   B reads A's Agent Card
   -> sees skills: ["research-prices", "web-browsing"]

2. REQUEST
   B -> A:  GET /skills/research-prices
   A -> B:  { skill manifest (L1) + abstract (L2) }

3. DECIDE
   B evaluates relevance to own tasks
   -> Yes: request full body
   -> No:  stop

4. TRANSFER
   B -> A:  GET /skills/research-prices?layer=full
   A -> B:  complete skill yaml (L1+L2+L3)

5. INSTALL
   B writes to local skill store
   B registers in profile.yaml
   B optionally re-shares to registry
```

### 7.2 Push Offer (supplementary)

```
A -> B:  POST /skills/offer
         {
           "skill_name": "research-prices",
           "reason": "Detected you are scraping product pages",
           "confidence": 0.85
         }

B -> A:  { "accepted": true }   -> proceed to Transfer step
         { "accepted": false }  -> stop
```

### 7.3 A2A Endpoint Additions

| Endpoint | Method | Response |
|----------|--------|----------|
| `/skills/{name}` | GET | Layer 1 + Layer 2 |
| `/skills/{name}?layer=full` | GET | Complete skill (L1+L2+L3) |
| `/skills/offer` | POST | Accept/decline response |

### 7.4 Provenance Tracking

Each installed skill records its origin:

```yaml
provenance:
  source: agent://research-agent        # Where it came from
  transferred_at: 2026-05-24T10:30:00Z
  original_publisher: agent:research-agent
  transfer_chain: [agent://research-agent]
```

Re-shared skills append to `transfer_chain`, creating an auditable skill propagation graph.

### 7.5 Trust Model

Trust is not deferred — it is the first decision made when a skill is installed. Section 2 defines the full three-tier trust model (Sandboxed / Verified / Trusted) and the defense-in-depth architecture. Every skill, regardless of install source, enters at a trust level that gates its capabilities:

- **Registry install with signed publisher** → Verified (checksum + signature verified)
- **Peer transfer** → Sandboxed (requires user approval + N days without incident to upgrade)
- **Agent-generated** → Sandboxed (requires review before promotion)
- **Local file install** → Verified if checksum matches registry; Sandboxed otherwise

All trust decisions are recorded in a `SkillTrustStore` (modeled on mur-commander's `TrustStore` at `engine/src/trust/store.rs`), persisted to `~/.mur/trust/skills.json` with 0o600 permissions, atomic writes, and constant-time checksum verification.

## 8. Agent-Generated Skills

### 8.1 Generation Triggers

1. **Manual**: `mur skill generate --from-session <session-id>`
2. **Auto-suggest**: Agent detects repeated task pattern >= 3 times, offers to extract
3. **Pattern promotion**: `mur skill from-pattern <pattern-name>` — promote a Stable/Canonical pattern to a skill

### 8.2 Generation Pipeline (Trace2Skill Pattern)

The state-of-the-art approach (Trace2Skill, arXiv 2603.25158; SkillForge, SIGIR 2026) uses a three-stage parallel pipeline rather than a single LLM pass:

```
Phase 1: Trajectory Generation
  Session recordings (mur in/out) → pool of success + failure trajectories

Phase 2: Parallel Multi-Agent Patch Proposal
  ├─ Success Analysts: extract reusable behavior patterns from each success trajectory
  └─ Error Analysts (multi-turn ReAct): diagnose root causes of failures
      across 4 dimensions: Knowledge, Tool, Clarification, Style

Phase 3: Conflict-Free Patch Consolidation
  → Hierarchical merge of all patches
  → Deduplication + conflict detection
  → Format validation
  → Single coherent skill.yaml output
```

Key findings from Trace2Skill that inform this design:
- **Cross-model transfer**: skills evolved by a 35B model improved a 122B model by +57.65pp — skills are transferable across scales
- **Parallel consolidation outperforms sequential**: 20× speedup, higher quality
- **Single comprehensive skill > retrieval-based experience banks**
- **Agentic error analysis > single-call LLM analysis** for robust patch generation
- **Skills generalize out-of-distribution**: spreadsheet skills transferred to Wikipedia table QA

### 8.3 Agent vs Human Output

Agents output canonical YAML. Humans can author in either format. They are equivalent and interconvertible:

```
Canonical YAML (agent output)  <->  Markdown frontmatter (human authoring)
       mur skill fmt --yaml               mur skill fmt --markdown
```

### 8.4 Closed-Loop Self-Evolution (SkillForge Pattern)

Beyond one-shot generation, skills participate in a continuous improvement loop (per SkillForge, SIGIR 2026):

```
Create → Execute → Evaluate → Diagnose → Optimize → Repeat
```

**Failure Analyzer**: batch-diagnoses execution failures across 4 dimensions:
- **Knowledge**: skill lacks domain information → enrich context section
- **Tool**: wrong tool or tool params → update procedure steps
- **Clarification**: ambiguous instructions → clarify variable descriptions
- **Style**: output format mismatch → adjust output templates

**Skill Optimizer**: rewrites skill with minimal-modification principle — only changes what's broken, preserving verified behavior. After 3 iterations, auto-evolved skills surpass human-expert-crafted quality (+9–12pp in cloud support domain).

**Evolution tracking**: each iteration records `evolution_event` (modeled on EvoMap's EvolutionEvent / mur's pattern lifecycle):
```yaml
evolution_log:
  - version: 1.0.0
    generation: 0
    source: human:david
  - version: 1.1.0
    generation: 1
    source: agent:researcher
    changes: "Added error handling for timeout, clarified variable descriptions"
    quality_score: 0.87  # vs 0.82 for v1.0.0
```

## 9. Skill Lifecycle & Technical Debt Management

Skill Technical Debt (STD) is recognized in 2026 as a distinct category of software defect. Skills "rot" as their underlying tools, APIs, and assumptions change — and agents silently produce worse results. Version constraints alone don't solve the entropy problem.

### 9.1 Lifecycle States

Adapted from mur's existing pattern lifecycle (`evolve/lifecycle.rs`) and maturity (`evolve/maturity.rs`):

```
Draft → Emerging → Stable → Canonical → Deprecated → Archived
```

| State | Criteria | Behavior |
|-------|----------|----------|
| **Draft** | Newly created/generated | Sandboxed trust; not injected automatically |
| **Emerging** | Used successfully >= 3 times | Injected with low priority; eligible for peer transfer |
| **Stable** | Used >= 10 times, effectiveness >= 0.6, age >= 7 days | Verified trust; registry-publishable |
| **Canonical** | Used >= 30 times, effectiveness >= 0.8, age >= 30 days, pinned | Trusted; injected with highest priority |
| **Deprecated** | Effectiveness < 0.3 OR no successful use in 90 days | Still usable but flagged; warning on install |
| **Archived** | Deprecated + 180 days | Read-only; removed from registry search |

### 9.2 Skill Decay (Entropy Management)

Adapted from mur's pattern decay (`evolve/decay.rs`):

- **Confidence decay**: `confidence * 0.5^(days_since_last_success / half_life)`
- **Half-life defaults**: Draft=14 days, Emerging=90 days, Stable=365 days, Canonical=730 days
- **Auto-demotion**: Skill drops a tier when confidence falls below threshold
- **Auto-archival**: confidence < 0.1 → archived
- **Pinned skills**: immune to decay (human override)
- **Effectiveness tracking**: each skill execution records success/failure; rolling 10-execution window

### 9.3 Health Checks

`mur skill doctor` — modeled on `mur agent doctor`:
- **Tool availability**: are referenced MCP tools still present?
- **Dependency freshness**: are all `requires` within supported version range?
- **Execution recency**: when was this skill last used successfully?
- **Failure rate**: last 10 executions success ratio
- **API drift**: do step descriptions reference outdated API patterns?

`mur skill doctor --fix` attempts auto-repair for low-severity issues (update dependency versions, refresh examples).

### 9.4 Consolidation

Adapted from mur's pattern consolidation (`evolve/consolidate.rs`):

- **Dedup detection**: cosine similarity >= 0.85 between two skills → flag for merge
- **Contradiction detection**: two skills give conflicting instructions for same trigger → flag
- **Orphan detection**: skill with zero uses in 180 days → suggest archival
- **Coverage gap**: common user task with no matching skill → suggest creation

## 10. Observability & Execution Tracing

A skill ecosystem needs internal execution observability — not just provenance of where a skill came from, but what happens when it runs.

### 10.1 Skill Execution Trace

Skill execution emits the **same** journal event types as the host (`step_started`, `step_completed`, `step_failed`, etc.) with skill-specific attributes. Skill steps are microsteps within host steps, producing ONE coherent journal that replay tooling can read end-to-end.

```jsonl
{"event":"step_started","plan_id":"P-8f3a","step_id":"step_001","microstep":"skill.research-prices.1","skill_id":"research-prices@1.1.0","skill_step":1,"tool":"browser.navigate","trust":"verified"}
{"event":"step_completed","plan_id":"P-8f3a","step_id":"step_001","microstep":"skill.research-prices.1","duration_ms":1230}
{"event":"step_started","plan_id":"P-8f3a","step_id":"step_001","microstep":"skill.research-prices.2","skill_id":"research-prices@1.1.0","skill_step":2,"tool":"browser.fill"}
{"event":"step_failed","plan_id":"P-8f3a","step_id":"step_001","microstep":"skill.research-prices.2","skill_id":"research-prices@1.1.0","skill_step":2,"error":"Element not found: #search-input","recovery_action":"retry","retry":1}
```

The `microstep` attribute uses the convention `skill.<skill_name>.<step_index>`. The host's `plan_id` and `step_id` remain the primary keys; skill steps are nested microsteps within the host step that loaded them.

Skill-specific lifecycle events (`skill_loaded`, `skill_skip_context_full`) are emitted as standalone events only when they don't correspond to a procedure step.

### 10.2 Trace Infrastructure (Reuse)

Mur already has the infrastructure:

| Existing Component | Reuse for Skill Observability |
|--------------------|------------------------------|
| `mur-agent-runtime/src/telemetry_writer.rs` (JSONL daily files) | Skill execution trace sink |
| `mur-common/src/telemetry.rs` (OTel GenAI constants) | Skill telemetry spans (`mur.skill.*`) |
| `mur-agent-runtime/src/hooks/telemetry.rs` (10-phase hook) | Skill lifecycle hooks |
| `mur-commander crates/engine/src/observability/` (collector + redaction) | Skill trace redaction + collection |
| `mur-core/src/session/mod.rs` (JSONL recording) | Skill session recording |
| `mur-core/src/conversations/audit.rs` (hash-chained audit) | Immutable skill execution audit |

### 10.3 Redaction Modes

Adapted from mur-commander `observability/redaction.rs`:
- **Full**: pass through (debug mode)
- **Redacted** (default): replace content bodies with SHA-256 digest + length metadata
- **MetadataOnly**: drop all content, keep counters and timing only

### 10.4 Skill Health Dashboard

Per-skill metrics viewable via `mur skill info <name> --metrics`:
- Execution count (total, last 7d, last 30d)
- Success rate (rolling 10-execution window)
- Average latency per step
- Most common failure modes
- Dependency health summary

## 11. MCP Deep Integration (Roadmap)

The initial design treats skills as procedural knowledge and MCP as tool execution — separate concerns. Long-term, a skill's execution substrate IS MCP: skills declare tool requirements, MCP servers provide them, and the runtime dynamically wires them.

### 11.1 Skill → MCP Binding

```yaml
# Future: skill declares MCP tool requirements
mcp_requirements:
  - tool_pattern: "browser.*"        # Any browser tool
    capability: network_http          # Required trust capability
    fallback: "builtin-http"          # Fallback if no MCP server provides this
  - tool_pattern: "filesystem.write.*"
    capability: write_file
```

This mirrors mur-commander's MCP capability system (`engine/src/mcp/trust.rs` lines 16-73): six capabilities (`ReadFile`, `ListTools`, `Search`, `WriteFile`, `ExecuteSafe`, `NetworkHttp`) mapped to trust levels.

### 11.2 MCP as the Skill Execution Substrate

EvoMap's GEP protocol conceptual stack: MCP (tool layer) → Skill (capability layer) → Evolution (adaptation layer). The mur skill ecosystem should follow the same layering:

```
Evolution Layer (M4+): mur evolve + self-evolution loop
    ↕
Skill Layer (M1-M3):   This design — authoring, sharing, composing
    ↕
Tool Layer (existing):  MCP servers registered per agent
```

### 11.3 Dynamic Tool Resolution

Skills shouldn't hardcode tool names — they should declare intent:
```yaml
steps:
  - description: Navigate to search page
    intent: web_navigate
    tool_hint: "browser.navigate"      # Preferred, falls back to any web_navigate provider
```

The runtime resolves `intent` to available MCP tools at execution time. This avoids skill breakage when MCP servers change.

## 12. CLI Surface Summary

### New: Global Skill Management

```
mur skill install <source>          # Install from registry/git/agent/file
mur skill remove <name>             # Uninstall a skill
mur skill list                      # List installed skills
mur skill show <name>               # Display full skill content
mur skill search <query>            # Search registry (+ --local flag)
mur skill info <name>               # Layer 1+2 summary (+ --full flag)
mur skill publish <path>            # Publish to registry
mur skill update <name>             # Update to latest version
mur skill validate <path>           # Validate skill format
mur skill fmt <path> [--markdown|--yaml]  # Convert between formats
mur skill generate --from-session <id>    # Generate skill from session recording
mur skill from-pattern <pattern>    # Promote pattern to skill
mur skill audit <name>              # Run full security scan on a skill
mur skill trust <name> --level <L>  # Promote/demote skill trust level
mur skill doctor [<name>]           # Health check (tool availability, decay, failures)
mur skill doctor --fix              # Auto-repair low-severity issues
mur skill evolve                    # Run full lifecycle pass across all skills
```

### Upgraded: Agent Skill Binding

```
mur agent skill add <agent> <skill>     # Bind existing skill to agent
mur agent skill remove <agent> <name>   # Unbind skill from agent
mur agent skill list <agent>            # List agent's skills
mur agent skill show <agent> <name>     # Show skill content
mur agent skill publish <agent> <name>  # Publish agent's skill to registry
```

## 13. Migration Path

### Phase 1 — Format Compatibility (non-breaking)
- `Skill` struct supports parsing from old markdown frontmatter
- Old markdown files auto-treated as `context` mode with Layer 2 only
- `mur skill validate` suggests upgrade but does not error on old format

### Phase 2 — Tooling Conversion
- `mur skill upgrade <name>` interactively converts old markdown to new format
- `mur sync` writes new-format skills to AI tool directories
- Four built-in skills updated to new format

### Phase 3 — Runtime Activation
- Agent runtime injects skill Layer 2 into system prompt
- Trigger matching enabled
- Peer transfer protocol endpoints online

### Built-in Skill Migration

| Current Skill | New Category | Trigger |
|---------------|-------------|---------|
| mur-context | context | session_start |
| mur-in | command | command: `/mur-in` |
| mur-out | command | command: `/mur-out` |
| mur-run | workflow | keyword: `mur run`, `/mur-run` |

## 14. Milestones

### M0 — Security Foundation + Data Model (HIGHEST PRIORITY)

**Data model:**
- `Skill` struct in `mur-common/src/skill.rs` with serde + validation (including `trust_level`, `capabilities_declared`, `publisher_signature`, `content_sha256` fields)
- Dual format parser (canonical YAML + markdown frontmatter)
- `~/.mur/skills/<name>/skill.yaml` storage
- `mur skill validate`
- Four built-in skills upgraded
- Backward-compatible old-format reader

**Security (MANDATORY — no skill system ships without this):**
- `SkillTrustStore` at `~/.mur/trust/skills.json` (0o600, atomic writes, constant-time checksum — modeled on mur-commander `engine/src/trust/store.rs`)
- Three-tier trust model (Sandboxed / Verified / Trusted) — reuse mur-commander `trust/level.rs`
- Skill content scanner: DDIPE prompt injection detection, 11 secret patterns (reuse `b0_helpers.rs` lines 151-179), executable content ban (reuse `muragent/executable_ban.rs`)
- Unicode NFC normalization + bidi detection (reuse mur-commander `constitution/signing.rs` SEC-14)
- Skill install-time SHA-256 pinning (reuse B0 rule 6 pattern from `b0_helpers.rs` lines 536-586)
- Publisher Ed25519 signature verification (reuse `identity.rs` + `character_card/signing.rs`)
- Skill capability declaration — each skill declares required entitlements; denied if exceeds trust level
- Malicious skill kill-switch: ability to revoke a skill globally by content hash

### M1 — CLI + Registry + Content Scanning
- `mur skill install/list/show/remove/search/info`
- Git-based registry: `mur-run/skill-registry` repo + index.yaml
- `mur skill publish` (human flow) — requires signed skill
- `mur skill audit <name>` — run full security scan on a skill
- `mur skill trust <name> --level verified` — promote trust level after review
- `mur agent skill add/remove/list/show` upgraded, CLI-compatible
- CI auto-validation for registry PRs (content scan + signature check)

### M2 — Runtime Injection + Sandboxing
- Agent runtime reads skills, injects Layer 2 into system prompt
- Token budget + priority logic + trust-level priority ordering (Trusted > Verified > Sandboxed)
- Trigger matching engine (command / keyword / session_start)
- Layer 3 on-demand loading
- Per-skill sandbox policy derived from trust level (reuse mur's `sandbox/` Landlock/SBPL/Job + mur-commander's `sandbox/limiter.rs`)
- Skill instruction boundary: `<skill-instruction source="..." trust="...">` wrapping (patterned after `<untrusted_tool_result>`)
- B0 hook chain applies to skill-triggered tool calls
- Skill SHA-256 drift detection at load time

### M3 — Composition + Agent Generation + Self-Evolution
- `requires:` dependency resolution and installation (with security audit of transitive dependencies)
- `skill.lock` lock file
- Circular dependency detection
- `mur skill generate --from-session` — Trace2Skill-style parallel multi-agent pipeline
- `mur skill from-pattern`
- Self-evolution loop: Failure Analyzer (4-dimension diagnosis) + Skill Optimizer (minimal-modification rewrites)
- Evolution tracking with `evolution_log`

### M4 — Peer Transfer + Observability
- A2A endpoints: `GET /skills/{name}`, `POST /skills/offer`
- Agent Card skill broadcast (existing field, upgraded content)
- Pull transfer flow + push offer flow
- Provenance recording + transfer chain
- `mur skill install agent://<name>`
- Peer-transferred skills auto-enter at Sandboxed trust
- Skill execution traces (JSONL daily files — reuse `telemetry_writer.rs`)
- `mur.skill.*` OTel telemetry spans
- `mur skill info <name> --metrics` — per-skill health dashboard
- Hash-chained skill execution audit (reuse `conversations/audit.rs`)

### M5 — Lifecycle Management + Consolidation
- Full lifecycle state machine (Draft→Emerging→Stable→Canonical→Deprecated→Archived)
- Confidence decay + auto-demotion + auto-archival
- `mur skill doctor` — health checks (tool availability, dependency freshness, failure rate, API drift)
- `mur skill doctor --fix` — auto-repair for low-severity issues
- Consolidation: dedup detection, contradiction detection, orphan detection, coverage gap analysis
- `mur skill evolve` — run full lifecycle pass across all installed skills

### M6 — MCP Deep Integration + Ecosystem
- Skill → MCP capability binding (`mcp_requirements` with tool patterns → trust capabilities)
- Dynamic tool resolution (`intent` + `tool_hint` → best available MCP tool)
- MCP as execution substrate for skill procedures
- Skill propagation graph visualization
- Registry web UI
- Skill ratings / usage statistics

### M7 — Cross-Agent Evolution (Future Platform)
- EvoMap-style genetic sharing: skills as "genes" that can be inherited, mutated, and recombined
- Population-level skill evolution: skills that perform well propagate; poor skills are pruned
- Credit/reputation incentive system for skill contributions
- Cross-agent skill performance benchmarking

## 15. Deferred to Future Design

- Paid/private skill registries (authentication, billing, private namespaces)
- Cross-platform skill compatibility matrix (Linux vs macOS vs Windows sandbox capability mapping)
- WASM sandbox for skills (mur-commander `sandbox/wasm.rs` via wasmtime — stronger isolation than OS-level only)
- Skill A/B testing framework (compare two versions of a skill on the same task across multiple runs)
- Federated registry protocol (registries that mirror and cross-validate each other)
- Skill "genes" — EvoMap-style genetic recombination of skill components
- Population-level evolution analytics (skill propagation graph, fitness metrics across the ecosystem)

**Items that were deferred in v1 but are now first-class:**
- ~~Trust/safety metadata~~ → Section 2 (Security & Supply Chain), M0 priority
- ~~Skill execution sandboxing~~ → Section 2.2 Layer 3, M2 milestone
- ~~MCP tool binding within skills~~ → Section 11 (MCP Deep Integration), M6 milestone
