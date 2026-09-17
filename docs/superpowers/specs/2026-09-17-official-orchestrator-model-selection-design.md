# Official Free Orchestrator with install-time model selection — design

Status: **approved**
Date: 2026-09-17

## Problem

MUR has a local `orchestrator` agent, but making it an official agent is more
than changing a profile label. Official agents are catalog items: they are
packaged in the private `official-sign` repository, signed by the official key,
distributed with an account-bound license, and installed through MUR's ordinary
agent importer.

The local orchestrator is not portable as-is. It names a machine-specific model,
paths, and identity. More importantly, an official orchestrator cannot assume
that every user has the same provider or model. MUR already supports a named
model registry and per-agent fallback chains, but official installation does not
currently resolve a package's model requirements against that registry.

The runtime fallback boundary is also too broad for the desired product. Today
all unrecognised HTTP 4xx responses, plus insufficient credit, advance to the
next model. For an orchestrator this can silently route around billing,
permission, or safety failures. The approved behavior is narrower: fail over
only when a model is unavailable or temporarily unhealthy, or when its context
window is too small.

## Product decision

The base official orchestrator is a **free** catalog item. It provides MUR's
fundamental multi-agent coordination capability without a subscription gate.
Pro value belongs in maintained fleet compositions, advanced governance and
approval templates, high-volume research configurations, team sharing, and
enterprise policy packs—not in removing the basic steering wheel.

`free` changes download entitlement only. It does not weaken signatures,
account-bound licensing, runtime entitlements, or agent permissions. It also
does not grant fewer model or tool capabilities than `pro`.

The package version starts at `1.0.0`. A published version is immutable. Any
later content or tier change requires a version bump; changing free to pro is a
product-policy migration, not an in-place metadata edit.

## Approved user experience

Installing `agents/orchestrator` asks the user to choose one of three policies:

1. **Capability first** — strongest suitable models first; prefer local among
   otherwise equivalent models.
2. **Cost first** — local, subscription-covered, then usage-billed models;
   compare known prices inside a billing class.
3. **Privacy first** — local models only.

The UI previews the resulting primary model, ordered fallback chain, and any
warnings before confirmation.

In a non-interactive CLI, the default is `capability-first`. Automation can make
the choice explicit:

```console
mur official install agents/orchestrator \
  --model-policy capability-first
```

Supported values are:

```text
capability-first
cost-first
privacy-first
```

For reproducible automation, explicit refs are also supported:

```console
mur official install agents/orchestrator \
  --model-ref claude_opus \
  --fallback ollama_qwen3 \
  --fallback openai_gpt5
```

Explicit selection takes precedence over policy selection, but it does not skip
existence or capability checks.

Other official agents and all official fleets retain their current install
behavior unless they opt into model requirements. The installer must not start
asking model questions for `researcher` merely because orchestrator needs them.

## Existing contracts

The design builds on four measured contracts.

### The shared installer owns no UI

`mur-core/src/official/install.rs:3-8` says:

> The function does the whole trust chain — download, verify the account-bound
> license fail-closed, persist it, then hand the bundle to the ordinary import
> paths (which re-verify signatures against that license) — and prints nothing,
> so callers own their own output.

Therefore CLI and Hub own prompting and rendering. The shared installer accepts
an explicit selection and returns a structured outcome.

### Profiles already support per-agent chains

`mur-common/src/agent.rs:77-84` defines `model_ref` and `fallback_chain`, with a
non-empty per-agent chain overriding the global chain. The package leaves both
unset; installation resolves and writes them for this machine.

### The registry has the ranking inputs

`mur-common/src/model.rs` provides provider, model, capabilities, route tier,
input/output prices, context window, billing mode, and catalog verification.
`ModelEntry::billing_or_inferred()` is the canonical compatibility path for old
registry entries. Unknown prices are unknown, never zero.

### Existing fallback mutation is fail-closed but not atomic

`mur-core/src/cmd/agent/model_resolve.rs:120-137` validates every fallback ref
before writing the profile. That validation rule is reused. Its direct
`std::fs::write` is not reused: installation needs one atomic rewrite of the
complete model selection.

## Architecture

There are three implementation tracks and two repositories:

| Track | Repository | Responsibility |
|---|---|---|
| Install-time selection | `mur` | Requirements, deterministic planner, CLI/Hub interaction, profile mutation |
| Runtime fallback safety | `mur` | Typed failure categories and the allowed failover boundary |
| Official content and publishing | `official-sign` | Portable package, catalog entry, catalog-driven CI, signed release |

They land in that order. The official artifact must not be published before a
released MUR installer can configure it.

### Front ends own interaction

The CLI and Hub:

1. fetch or inspect the selected catalog item;
2. discover whether it declares install-time model requirements;
3. present policy choices and a plan preview;
4. pass an explicit `ModelSelection` to the shared installer;
5. render the returned outcome or error.

A non-interactive CLI constructs `Automatic(CapabilityFirst)` without prompting.
The Tauri command receives the Hub's selected policy; it does not infer what the
React UI meant.

### Core owns planning and mutation

The core exposes the following semantic data structures (field names may follow
repository conventions, but the separation and information carried are fixed):

```rust
pub struct OfficialInstallOptions {
    pub model_selection: ModelSelection,
}

pub enum ModelSelection {
    Automatic(ModelSelectionPolicy),
    Explicit {
        primary: String,
        fallbacks: Vec<String>,
    },
    Unchanged,
}

pub enum ModelSelectionPolicy {
    CapabilityFirst,
    CostFirst,
    PrivacyFirst,
}

pub struct ModelRequirements {
    pub chat: bool,
    pub tools: bool,
    pub minimum_context_window: Option<u64>,
}

pub struct ModelSelectionPlan {
    pub primary: String,
    pub fallbacks: Vec<String>,
    pub warnings: Vec<ModelSelectionWarning>,
}

pub struct InstallOutcome {
    pub item_id: String,
    pub agent_name: Option<String>,
    pub model_selection: Option<ModelSelectionPlan>,
}
```

The type spelling may follow repository conventions, but the separation may not:
planning is a pure deterministic operation; prompting is outside core; profile
mutation is inside the trusted install path.

The planner API is independently testable:

```rust
pub fn plan_model_selection(
    registry: &ModelRegistry,
    requirements: &ModelRequirements,
    policy: ModelSelectionPolicy,
) -> Result<ModelSelectionPlan>;
```

### Package requirements, not package preferences

The orchestrator package declares minimum requirements such as chat and tools.
It does **not** encode a preferred vendor, a model registry key, or a global
routing policy. The exact manifest representation is chosen during planning
against the official package schema, but it must be signed package metadata,
not inferred from the item name and not hidden in prompt prose.

Installers that do not understand the requirements metadata must fail clearly
rather than installing an unusable profile. This makes the minimum supported
MUR version an explicit release concern.

## Candidate eligibility

A registry entry is eligible when all of these hold:

1. Its registry key exists in the loaded `~/.mur/models.yaml`.
2. Provider and model fields are non-empty and parseable by the existing model
   resolution path.
3. It supports chat.
4. It supports tools, or it is a legacy entry with no capability declaration.
5. It does not explicitly contradict a package requirement.
6. For privacy-first, `billing_or_inferred()` is `Local`.

An explicitly declared capability list that omits `tools` is ineligible when
`tools` is required. An empty legacy capability list remains eligible for
backward compatibility, but yields an `UnverifiedToolCapability` warning.

Privacy-first uses the canonical billing inference. A loopback `base_url` alone
does not prove locality; the existing code deliberately treats unknown
OpenAI-compatible endpoints as usage-billed unless the entry says otherwise.

A missing `context_window` is not fabricated. It remains eligible when no hard
minimum is declared, ranks below a known sufficient window where the policy
uses window size, and produces a warning. If the package declares a hard
minimum, an unknown window cannot prove eligibility and is excluded.

## Deterministic ranking

Every policy applies the same eligibility gate, then a policy-specific total
ordering. A final registry-key comparison guarantees reproducible CLI, Hub, and
test results.

### Capability first

Order by:

1. effective route tier, frontier before local/cheap;
2. known context window, larger first;
3. local billing before non-local when otherwise equivalent;
4. catalog-verified before unverified;
5. explicitly declared tools before legacy unknown;
6. registry key ascending.

The exact mapping of old or absent `RouteTier` values must use the existing
routing inference rather than inventing a second provider table.

### Cost first

Order by:

1. billing class: local, subscription, usage-billed;
2. within usage-billed, known effective cost lower first;
3. known price before unknown price; unknown is never treated as zero;
4. capability tier and context window, stronger/larger first;
5. catalog verification and explicit tools support;
6. registry key ascending.

Within usage-billed models, compare a fixed projected request of **4,000 input
tokens and 1,000 output tokens** using `effective_costs()`. The absolute token
count is only a normalization; the decision is the explicit 4:1 input/output
weight. This matches an orchestration turn's prompt-heavy shape without
pretending that an install-time planner knows the future task. If only one side
of a legacy price is known through `effective_costs()`, its existing fallback
semantics apply. Tests freeze this formula.

### Privacy first

Exclude non-local entries, then order by:

1. capability tier;
2. known context window, larger first;
3. catalog verification;
4. explicit tools support;
5. registry key ascending.

### Chain size

The planner writes at most **three fallbacks** (four candidates including the
primary). The runtime currently has no chain-length limit, so this product bound
prevents an install from turning a large registry into a long, expensive cascade.
The primary is never duplicated in fallbacks, and fallback refs are unique.

## Installation transaction

For an agent with requirements, installation proceeds as follows:

1. Authenticate and download the bundle plus license.
2. Verify the account-bound license fail-closed.
3. Verify and inspect the signed package far enough to obtain model
   requirements without installing it.
4. Load the model registry and build or validate the selection plan.
5. If planning fails, stop before creating or replacing an agent profile.
6. Persist the valid license.
7. Hand the archive and selection to the ordinary importer. It validates again,
   extracts into staging, applies existing host-local safety rewrites, and then
   re-loads the staged profile.
8. Revalidate every selected ref against the current registry, set `model_ref`
   and the fallback chain, synchronize the legacy inline `model` block from the
   primary entry, and atomically commit the fully configured staged agent.
9. Return `InstallOutcome` to the caller.

There is a race between planning and profile mutation because users or another
process can edit `models.yaml`. Revalidation at step 8 is mandatory. If any ref
has disappeared, no partial model rewrite is committed.

The installer must preserve the ordinary importer's existing collision and
replacement semantics. This design does not create a second unpacker.

The final profile rewrite uses the shared atomic profile saver. More importantly,
model configuration is folded into the ordinary importer **before its commit**:
the importer prepares the validated payload in a sibling staging directory,
applies egress downgrades and model selection there, then atomically swaps it
into the agent path. For an update it first renames the current agent directory
to a sibling backup, promotes staging, and removes the backup only after trust
state is saved; on any failure it restores the backup. For a fresh install it
removes the promoted directory on a post-promotion failure. The existing `data/`
directory is preserved across the swap with the same semantics as today's
`clear_except_data`. This replaces today's destructive in-place update in
`mur-common/src/muragent/installer.rs:167-180`; a second post-install mutation
would not be sufficient because it cannot faithfully restore files removed by
`clear_except_data`.

A verified license may remain after a no-model failure. It grants no executable
capability and avoids needlessly discarding a valid account-bound artifact.
Temporary bundles are removed by their temporary directory.

## No-candidate behavior

The installer never:

- downloads a model;
- creates a provider entry;
- writes an API key;
- changes the global default or fallback chain;
- silently changes the chosen policy;
- falls back to stub echo;
- installs an orchestrator that is known not to start.

Capability/cost selection reports the missing requirements and points the user
to the existing model setup flow. Privacy-first specifically says that no local
model is configured and that cloud fallback is disabled by policy. Hub remains
on the policy step and offers its existing model-configuration route.

## Runtime fallback boundary

The runtime uses a closed allow-list for advancing the chain.

### Retry, then advance

- connection failure;
- timeout;
- rate limit;
- server 5xx.

### Advance immediately

- model not found or unavailable;
- structured context-window exceeded.

### Stop

- authentication or authorization failure;
- insufficient credit;
- permission denial;
- safety-policy refusal;
- unsupported capability not established as model-specific availability;
- malformed request or request-builder error;
- invalid/unparseable response;
- any unclassified 4xx;
- any failure after user-visible answer content has begun streaming.

The existing behavior in `mur-agent-runtime/src/llm/mod.rs:270-361` advances on
`InsufficientCredit` and every `Rejected` 4xx. That is deliberately changed.
At minimum the typed error model gains distinctions equivalent to:

```text
ContextExceeded
PermissionDenied
SafetyPolicyRejected
UnsupportedCapability
```

Provider adapters classify using HTTP status plus structured provider error
codes. They must not use loose substring matching that could turn a safety or
permission refusal into context overflow. Unknown errors stop. This errs on the
side of a loud, actionable failure instead of silently changing provider or
billing source.

Fallback before any output is safe because the next candidate can produce the
whole answer. Once answer content has been emitted, changing candidates risks a
duplicate or contradictory continuation and therefore stops.

When all allowed candidates fail, the runtime retains the current principle:
report every attempted candidate and reason, while leading with the most
actionable failure.

## CLI flow

Interactive install:

```text
Choose orchestrator's model policy:

  › Capability first
    Cost first
    Privacy first

Primary      claude_opus
Fallback 1   ollama_qwen3
Fallback 2   openai_gpt5

Confirm install? [Y/n]
```

Warnings appear directly beneath the affected candidate. Cancellation writes no
agent state. A non-TTY invocation does not block waiting for input and states
that it used the capability-first default in the caller-owned output.

CLI parsing rejects combining `--model-policy` with `--model-ref`. Repeated
`--fallback` requires `--model-ref`. Fleet items and agents without requirements
reject model-selection flags rather than silently ignoring them.

## Hub flow

`SpecOfficial.tsx` currently invokes `official_install` with only `id`. For an
item with model requirements it becomes a two-stage flow:

1. select the official item;
2. select policy and preview the plan;
3. confirm installation.

The backend exposes a plan command or equivalent DTO so React does not
reimplement sorting. The install command receives the selected policy or
explicit plan identity and replans/revalidates in core; a preview is not an
authority because `models.yaml` may change between preview and confirmation.

Hub displays:

- primary and fallback refs;
- unverified tool capability;
- unknown or stale price data;
- unknown context window;
- no local candidate for privacy-first.

All copy is localized through the existing i18n system.

## Official package

The private `official-sign` repository adds:

```text
agents/orchestrator/
├── profile.yaml
└── prompt.md
```

The profile:

- has a stable package identity and version `1.0.0`;
- has no personal owner or identity;
- contains no absolute filesystem path;
- contains no local socket path such as `/tmp/a.sock`;
- contains no registry `model_ref` or fallback entries;
- contains no fixed Claude, OpenAI, Ollama, or other vendor preference;
- declares only portable transport settings;
- grants the minimum A2A capabilities and entitlements needed to delegate and
  observe work;
- ships no secret or provider credential;
- declares signed model requirements for chat and tools.

The exact orchestrator prompt and entitlement list are reviewed as official
content. Filesystem, process, and network access are denied unless the
orchestrator itself—not a delegated worker—has a demonstrated need. Delegation
must not become a way to grant the orchestrator every worker permission.

Catalog entry:

```yaml
- id: agents/orchestrator
  kind: agent
  name: orchestrator
  version: 1.0.0
  tier: free
  description: "Official multi-agent task orchestrator with safe model fallback."
```

The current `researcher` profile hard-codes an Ollama model and a `/tmp` socket.
That remains a separate compatibility cleanup; orchestrator does not copy those
choices.

## Catalog-driven publishing

`official-sign/.github/workflows/publish.yml` currently hard-codes
`agents/researcher` in dry-run, state detection, signing, release creation,
attestation, and commit messages. It becomes item-driven.

### Pull-request dry-run

For every catalog item:

1. validate unique id/kind/name/version tuples;
2. validate that the source directory exists;
3. validate catalog version against package profile version;
4. scan official profiles for secrets and non-portable fields;
5. build with a throwaway Ed25519 key;
6. verify the resulting manifest, signature, package type, and index entry;
7. verify that rebuilding/upserting does not violate version immutability.

The portability scan rejects at least absolute paths, owner/user identity,
registry model refs, fallback refs, credentials, and machine-specific socket
bindings. It is a real validator in the signing tool or a checked script, not a
collection of fragile workflow greps.

### Manual publish

`workflow_dispatch` requires an `item_id`. The workflow looks up kind, name,
version, source directory, artifact extension, release tag, and index identity
from `catalog.yaml`. It rejects an id not present in the catalog.

One dispatch publishes one item. It preserves the existing three recovery
states:

- `already-published`: immutable release and index entry both exist;
- `recover`: release exists but index entry is missing;
- `fresh`: no release exists.

Recovery may regenerate and commit the index but never overwrite an existing
release asset. The real official key remains in the protected signing
environment. Provenance remains best-effort only while the private repository's
plan cannot support it; the workflow emits an explicit warning when absent.

## Dashboard consistency

The server dashboard currently labels the entire Official Agents category as
`pro`, while catalog items can be free. Before orchestrator is announced, list
and detail surfaces must render each item's own tier. Category copy may describe
curation but must not imply that all official agents require Pro.

This is a presentation correction, not a change to server entitlement checks.
The download endpoint remains authoritative: free items require authentication
and a valid account-bound license but skip the active-subscription gate.

## Testing

### Planner unit tests

- capability-first selects the strongest eligible tier;
- equivalent capability prefers local;
- larger known context wins where specified;
- cost-first orders local, subscription, then usage-billed;
- lower known usage cost wins;
- unknown cost is never zero and ranks behind known cost in its class;
- privacy-first excludes all non-local entries;
- an explicit capability list without tools is excluded;
- an empty legacy capability list is accepted with a warning;
- an unknown hard-required context window is excluded;
- catalog verification and explicit tools support break ties;
- registry key produces stable final ordering;
- primary is absent from fallbacks and refs are unique;
- chain length is bounded.

### Installer tests

- agents without requirements preserve current behavior;
- interactive CLI passes the chosen policy;
- non-TTY CLI defaults to capability-first;
- conflicting or irrelevant flags are rejected;
- Hub and CLI receive the same plan for the same registry;
- explicit refs are validated for existence and capability;
- no candidate creates no agent;
- privacy-first never inserts a cloud candidate;
- a ref disappearing before commit fails without a partial profile rewrite;
- primary updates both `model_ref` and the legacy inline `model` block;
- fallback selection does not alter global model settings;
- profile replacement is atomic and rollback behavior is verified;
- license and package signature checks remain fail-closed;
- existing name-collision behavior is preserved.

### Runtime tests

- 429, timeout, connection failure, and 5xx retry then advance;
- model-not-found and structured context overflow advance immediately;
- 401/403, insufficient credit, permission denial, and safety refusal stop;
- unknown 4xx stops;
- malformed request and invalid response stop;
- no candidate switch occurs after streamed answer content;
- exhausted-chain output lists every candidate and reason and leads with the
  most actionable error;
- provider-specific structured codes map to the common typed errors.

### Official publishing tests

- dry-run builds both researcher and orchestrator from the catalog;
- duplicate ids fail;
- missing source directories fail;
- catalog/profile version mismatch fails;
- forbidden host-specific profile fields fail;
- same version with different bytes remains refused;
- selected-item dispatch derives all paths and names from the catalog;
- recover mode never overwrites an existing release;
- orchestrator bundle contains no local model, path, identity, or secret.

### Dashboard tests

- a free official agent renders `free` even when another item is pro;
- a pro official item still renders `pro`;
- category metadata does not override item metadata.

## Rollout

1. Land model requirements, planner, installer options, CLI/Hub flows, and
   atomic profile configuration in `mur`.
2. Land the stricter runtime error taxonomy and fallback tests in `mur`.
3. Release a MUR version that understands orchestrator requirements.
4. Land the portable orchestrator package and catalog-driven CI in
   `official-sign`.
5. Dry-run all catalog items with the throwaway key.
6. Manually dispatch `agents/orchestrator`, inspect the signed artifact and
   generated index, then publish.
7. Verify install in four environments: mixed local/cloud, cloud-only,
   local-only, and no eligible model.
8. Correct dashboard tier rendering before public announcement.

## Out of scope

- Automatically installing or downloading models.
- Editing API credentials or the global model registry during official install.
- Task-by-task dynamic routing inside the orchestrator prompt.
- Pro fleet content, team sharing, enterprise governance, or SLA policy.
- Converting every existing official agent to install-time model selection.
- Changing already-installed agents when catalog tier changes.
- Bypassing a permission, billing, safety, or authentication failure by trying
  another provider.

## Acceptance criteria

The feature is complete when:

1. `agents/orchestrator` is a signed free catalog item with no host-specific
   configuration.
2. Interactive CLI and Hub ask for capability, cost, or privacy policy and show
   the core-generated plan before installation.
3. Non-interactive CLI deterministically defaults to capability-first.
4. The installed profile has a valid primary and bounded ordered fallback chain
   drawn only from the user's existing registry, without modifying global model
   settings.
5. No eligible model leaves no runnable or partially configured orchestrator.
6. Runtime fallback occurs only for the approved availability, transient, and
   context-capacity failures, and never after output begins.
7. Official CI builds catalog items generically and manual publish targets one
   catalog id without hard-coded researcher paths.
8. Free/pro labels come from each catalog item on user-facing surfaces.
9. Unit, integration, runtime, package, and publishing tests above pass.
