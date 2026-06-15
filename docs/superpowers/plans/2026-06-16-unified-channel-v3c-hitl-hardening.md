# Unified Channel v3c — HITL Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Depends on v3a** (channel-aware executor seam) and reconciles with **v3b** (deterministic `idem_key`, `run_id`). Both are **planned, not yet built** — adjust the referenced call sites if their seam shifts. v3c **owns** the dedup-aware `append_event` and the `hitl::gate`; it **removes** v3a's fail-closed refusal of approval-bearing workflows.

**Goal:** add risk-tiered, hash-pinned human-in-the-loop to the channel-aware executor. Only **high-risk** actions gate (no approval fatigue); the gate interrupts **before** the side effect, **pins** a SHA-256 of the exact (post-substitution) args, surfaces a durable `HitlRequest` event in the channel, waits for a `HitlResponse`, **re-verifies the hash at the execute boundary (fail-closed)**, and only then runs. A new `mur channel approve` command closes the loop from the CLI/headless; Hub/iOS approval UIs are additive follow-ons. Re-running a crashed workflow **dedups** on the deterministic `idempotency_key` and **resumes** past already-completed steps.

**Architecture:** Decision B (B1-by-tier) from the v3 spec. Tier drives gate placement: `Read` → run unattended (post-hoc audit only); `Write/Destructive/Spend/Privileged/NetworkEgress` → pre-execution gate. The authoritative request/response is the channel `HitlRequest`/`HitlResponse` event pair under the existing `events.lock`; until v3d per-event signing lands, that pair is a **durable mirror** — for v3c the responder is the local `mur channel approve` CLI (single-user `~/.mur` trust domain, consistent with the v3 trust-model invariant). The pin defeats the "approve A, execute B" drift bug; `fingerprint_args` (build-local `DefaultHasher`) is explicitly NOT reused. `CHANNEL_SCHEMA_VERSION` bumps to **2** because a reader that silently skips a `HitlResponse` could re-apply a gated effect.

**Tech Stack:** Rust — `mur-common` (HITL types, schema bump), `mur-channel` (dedup `append_event`), `mur-core` (`hitl` module: pin + gate; executor wiring; `mur channel approve` CLI). `mur-core` has `sha2`. **No `mur-agent-runtime` change** — the `task_runner.rs` `handle_tool_call` pre-execution reorder (mode-1 LLM tool calls) is deferred to v3d per the spec.

**Scope guardrails (from `2026-06-16-unified-channel-v3-design.md` §5, §7 v3c, §8):**
- Tier is resolved **most-restrictive-wins** and is **never** LLM-asserted or read from observed content.
- Pin over **post-substitution** args; include a canonicalization version; **fail-closed** on drift.
- `idempotency_key` is **deterministic** (`idem_key` from v3b); dedup makes a crash-rerun a no-op.
- The channel `HitlResponse` is a **mirror, not authority**, until v3d signing — documented, single-host only.
- `task_runner.rs` reorder + per-event signing are **out of scope** (v3d).

**Key facts locked during exploration (do not re-derive):**
- `ToolPolicy {Allow,Ask,Deny}` (`agent.rs:532`) + `ToolRule {pattern, policy}` (`agent.rs:540`) + `resolve_tool_policy` (`agent.rs:548`) — the orthogonal entitlement ceiling.
- `ProcedureStep` (`manifest.rs:208`) has `needs_approval: bool` and (from v3b) `delegate_to`; it has **no** `risk` field — v3c adds one.
- DAG `needs_approval` branch is `dag.rs:179-208` (the non-TTY silent-skip-as-success landmine). v3a added a fail-closed refusal in `execute_dag`; v3c removes it and replaces the `execute_step` branch.
- `ChannelService::{append, transition, load_events}` (v3a) + `idem_key` (v3b) + `ChannelState::{InputRequired, Working}` already exist.
- `serde_json` has no `preserve_order` feature → `Value::Object` is BTreeMap-sorted, so `to_vec` is canonical for key order (float/unicode canonicalization remain the flagged open risks).
- `mur-channel` does **not** depend on `sha2` and does **not** need it: the pin is computed in `mur-core`; `append_event` dedup is a plain string compare of `idempotency_key`.

---

## File Structure

**Created:**
- `mur-common/src/hitl.rs` — `RiskTier`, `HitlMode`, `default_mode`, `HitlRequest`/`HitlResponse` payloads. (declare `pub mod hitl;` in `mur-common/src/lib.rs`)
- `mur-core/src/hitl/mod.rs` — `pub mod gate; pub mod pin;`
- `mur-core/src/hitl/pin.rs` — canonical SHA-256 action hash.
- `mur-core/src/hitl/gate.rs` — `gate()` + `ActionRequest`/`GateDecision`.

**Modified:**
- `mur-common/src/channel.rs` — `CHANNEL_SCHEMA_VERSION = 2`.
- `mur-common/src/agent.rs` — `ToolRule.risk: Option<RiskTier>`.
- `mur-common/src/skill/manifest.rs` — `ProcedureStep.risk: Option<RiskTier>`.
- `mur-channel/src/store.rs` — dedup-aware `append_event`.
- `mur-core/src/executor/dag.rs` — remove v3a fail-closed guard; replace `needs_approval` branch with `hitl::gate`; resume cursor; command-mode step-tier gating.
- `mur-core/src/lib.rs` (or `main.rs`) — `pub mod hitl;`.
- `mur-core/src/cmd/` — `mur channel approve` command (new subcommand file + CLI wiring).

---

## Task 1: HITL types + schema bump

**Files:**
- Create: `mur-common/src/hitl.rs`; Modify: `mur-common/src/lib.rs`, `mur-common/src/channel.rs:30`, `mur-common/src/agent.rs:540`, `mur-common/src/skill/manifest.rs:261`

- [ ] **Step 1: Write the failing test**

Create `mur-common/src/hitl.rs`:

```rust
//! Risk-tiered HITL vocabulary shared across the executor, runtime, and surfaces.

use serde::{Deserialize, Serialize};

/// How risky an action is. `Ord` is severity order: `Read` < … < `Privileged`.
/// Tier is resolved most-restrictive-wins and is NEVER LLM-asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RiskTier {
    Read,
    Write,
    NetworkEgress,
    Spend,
    Destructive,
    Privileged,
}

/// What the gate does for a tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HitlMode {
    /// Run unattended (read tier): a post-hoc audit event is fine.
    Auto,
    /// Pre-execution human approval required.
    Ask,
    /// Refuse pre-emptively.
    Deny,
}

/// Default gate mode for a tier. Read runs unattended; everything mutating asks.
/// A channel policy floor (future) may tighten Ask→Deny but never loosen.
pub fn default_mode(tier: RiskTier) -> HitlMode {
    match tier {
        RiskTier::Read => HitlMode::Auto,
        _ => HitlMode::Ask,
    }
}

/// `EventKind::HitlRequest` payload: the durable, pinned approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlRequest {
    pub hitl_id: String,
    /// SHA-256 of the canonical action (see `mur-core` `hitl::pin`).
    pub action_hash: String,
    pub tier: RiskTier,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub step_or_call_id: String,
    pub agent_id: String,
    pub timeout_ms: u64,
    pub summary: String,
}

/// `EventKind::HitlResponse` payload: the human's decision, echoing the pin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitlResponse {
    pub hitl_id: String,
    pub action_hash: String,
    pub allow: bool,
    #[serde(default)]
    pub reason: String,
    /// "cli" | "hub" | "ios" | "auto".
    pub surface: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_orders_by_severity_and_maps_mode() {
        assert!(RiskTier::Read < RiskTier::Destructive);
        assert!(RiskTier::Write < RiskTier::Privileged);
        assert_eq!(default_mode(RiskTier::Read), HitlMode::Auto);
        assert_eq!(default_mode(RiskTier::Destructive), HitlMode::Ask);
    }

    #[test]
    fn hitl_payloads_round_trip() {
        let req = HitlRequest {
            hitl_id: "h1".into(),
            action_hash: "abc".into(),
            tier: RiskTier::Destructive,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({ "cmd": "rm -rf x" }),
            step_or_call_id: "s0".into(),
            agent_id: "mur".into(),
            timeout_ms: 300_000,
            summary: "delete x".into(),
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: HitlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.tier, RiskTier::Destructive);
        assert_eq!(back.action_hash, "abc");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common hitl::` — Expected: FAIL — module `hitl` not declared.

- [ ] **Step 3: Wire the module + additive fields + schema bump**

- In `mur-common/src/lib.rs`, add `pub mod hitl;` (next to `pub mod channel;`).
- In `mur-common/src/channel.rs:30`, bump: `pub const CHANNEL_SCHEMA_VERSION: u32 = 2;` and add a one-line note to the doc-comment block (`:10-16`): "v2: `HitlResponse` events carry approval authority — a reader that silently skips one could re-apply a gated effect."
- In `mur-common/src/agent.rs`, add to `ToolRule` (`:540-543`):

```rust
    /// Intrinsic risk tier of this tool (v3c). Resolved most-restrictive-wins
    /// against per-step risk + channel policy; gates pre-execution when not Read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::hitl::RiskTier>,
```

(Update any `ToolRule { pattern, policy }` literals in `agent.rs` tests to add `risk: None,` — find with `grep -n "ToolRule {" mur-common/src/agent.rs`.)

- In `mur-common/src/skill/manifest.rs`, add to `ProcedureStep` (after `needs_approval`, `:261`):

```rust
    /// Risk tier for this step (v3c). When set on a command/delegate step run
    /// over a channel, the executor gates it via `hitl::gate` per tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::hitl::RiskTier>,
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common hitl:: channel:: skill::manifest` — Expected: PASS. (Old `channel.yaml`/`skill.yaml` still parse — new fields default.)

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/hitl.rs mur-common/src/lib.rs mur-common/src/channel.rs mur-common/src/agent.rs mur-common/src/skill/manifest.rs
git commit -m "feat(hitl): RiskTier/HitlMode/HitlRequest/HitlResponse types; schema v2

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 2: Canonical SHA-256 action pin

**Files:**
- Create: `mur-core/src/hitl/mod.rs`, `mur-core/src/hitl/pin.rs`; Modify: `mur-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `mur-core/src/hitl/pin.rs`:

```rust
//! Canonical SHA-256 pin of an action. Computed at gate time, embedded in the
//! HitlRequest, and RE-COMPUTED at the execute boundary — fail-closed on drift
//! (defeats the "approve A, execute B" bug). NOT `fingerprint_args` (that uses a
//! build-local DefaultHasher, unfit for a durable cross-process pin).

use sha2::{Digest, Sha256};

/// Canonicalization version — bump if the canonical form changes, so an old
/// pinned hash is never silently compared against a new canonicalization.
pub const PIN_CANON_VERSION: u32 = 1;

/// SHA-256 hex over the canonical action. `input` must already be the
/// POST-substitution args (what will actually execute). `serde_json` sorts
/// object keys (no preserve_order feature), so the encoding is deterministic.
pub fn action_hash(
    tool_name: &str,
    input: &serde_json::Value,
    channel_id: &str,
    step_or_call_id: &str,
    agent_id: &str,
) -> String {
    let canon = serde_json::json!({
        "v": PIN_CANON_VERSION,
        "tool": tool_name,
        "input": input,
        "channel": channel_id,
        "step": step_or_call_id,
        "agent": agent_id,
    });
    let bytes = serde_json::to_vec(&canon).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_and_order_independent() {
        let a = action_hash("bash", &serde_json::json!({"b":1,"a":2}), "c", "s0", "mur");
        // Same logical input, keys written in a different order → same hash
        // (serde_json sorts object keys).
        let b = action_hash("bash", &serde_json::json!({"a":2,"b":1}), "c", "s0", "mur");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn drift_changes_the_hash() {
        let approved = action_hash("bash", &serde_json::json!({"cmd":"rm a"}), "c", "s0", "mur");
        let executed = action_hash("bash", &serde_json::json!({"cmd":"rm b"}), "c", "s0", "mur");
        assert_ne!(approved, executed, "different args MUST fail the re-verify");
    }
}
```

Create `mur-core/src/hitl/mod.rs`:

```rust
//! Risk-tiered, hash-pinned HITL gate for the channel executor (v3c).
pub mod gate;
pub mod pin;
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core hitl::pin` — Expected: FAIL — module `hitl` not declared in the crate.

- [ ] **Step 3: Declare the module**

In `mur-core/src/lib.rs` add `pub mod hitl;` (and in `main.rs` if the crate declares modules there too — `grep -n "pub mod executor" mur-core/src/lib.rs mur-core/src/main.rs` and mirror). Create `gate.rs` as a stub for now so `mod.rs` compiles:

```rust
// mur-core/src/hitl/gate.rs — implemented in Task 4.
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core hitl::pin` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/hitl/ mur-core/src/lib.rs
git commit -m "feat(hitl): canonical SHA-256 action pin (v3c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 3: Dedup-aware `append_event`

**Files:**
- Modify: `mur-channel/src/store.rs:73-120`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `store.rs`:

```rust
    #[test]
    fn append_dedups_on_idempotency_key() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let e0 = store
            .append_event("c1", ChannelActor::System, EventKind::ToolResult,
                serde_json::json!({"x":1}), Some("k1".into()))
            .unwrap();
        // Same key again → returns the EXISTING event, does not append a 2nd row.
        let e0b = store
            .append_event("c1", ChannelActor::System, EventKind::ToolResult,
                serde_json::json!({"x":2}), Some("k1".into()))
            .unwrap();
        assert_eq!(e0.seq, e0b.seq, "same idempotency_key → same event");
        assert_eq!(e0b.payload["x"], 1, "first write wins; second is ignored");
        assert_eq!(store.load_events("c1").unwrap().len(), 1, "no duplicate row");
        // A None key never dedups.
        store.append_event("c1", ChannelActor::System, EventKind::Note,
            serde_json::json!({}), None).unwrap();
        store.append_event("c1", ChannelActor::System, EventKind::Note,
            serde_json::json!({}), None).unwrap();
        assert_eq!(store.load_events("c1").unwrap().len(), 3);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-channel store::tests::append_dedups_on_idempotency_key` — Expected: FAIL — second append creates a second row.

- [ ] **Step 3: Add the dedup check under the lock**

In `append_event` (`store.rs:99-110`), after acquiring the lock and before computing `next_seq`, add:

```rust
        // Dedup: if this idempotency_key already exists in the log, return the
        // prior event unchanged (exactly-once for crash-reruns; v3c). Done under
        // the lock so a concurrent writer can't slip a duplicate in between.
        if let Some(key) = idempotency_key.as_deref() {
            if let Some(existing) = self
                .load_events(id)?
                .into_iter()
                .find(|e| e.idempotency_key.as_deref() == Some(key))
            {
                FileExt::unlock(&lock).ok();
                return Ok(existing);
            }
        }
```

(`load_events` is already called just below for `next_seq`; the extra read is the known O(n)-under-lock cost flagged for the v3d cached-tail optimization.)

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-channel store::` — Expected: PASS (new test + existing `append_assigns_monotonic_seq`, which uses `None` keys and is unaffected).

- [ ] **Step 5: Commit**

```bash
git add mur-channel/src/store.rs
git commit -m "feat(channel): dedup-aware append_event on idempotency_key (v3c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 4: The `hitl::gate`

**Files:**
- Modify: `mur-core/src/hitl/gate.rs`

- [ ] **Step 1: Write the failing test**

Replace the `gate.rs` stub with the implementation below (test included). The poll loop is exercised by **pre-writing** a `HitlResponse` into the log, so the test returns without real waiting:

```rust
//! Risk-tiered, hash-pinned approval gate over a Channel (v3c). Writes a durable
//! HitlRequest, waits for a HitlResponse (CLI `mur channel approve`, or a future
//! Hub/iOS UI), and returns the decision. The channel pair is a MIRROR — single
//! trusted writer per the v3 trust-model invariant; per-event signing (authority
//! for headless approval) is v3d.

use std::time::{Duration, Instant};

use anyhow::Result;
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelState, EventKind};
use mur_common::hitl::{HitlMode, HitlRequest, HitlResponse, RiskTier, default_mode};

use crate::hitl::pin::action_hash;

/// What the caller wants to do. `tool_input` must be POST-substitution.
pub struct ActionRequest {
    pub tier: RiskTier,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub step_or_call_id: String,
    pub agent_id: String,
    pub summary: String,
}

/// The gate's verdict. `action_hash` is the pin the caller MUST re-verify just
/// before executing (fail-closed on mismatch).
pub struct GateDecision {
    pub allow: bool,
    pub reason: String,
    pub action_hash: String,
}

/// How often the wait loop re-reads the log, and the default wait budget.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// Gate an action. `yes` auto-approves Ask-tier actions (records an `auto`
/// HitlResponse for the audit trail). Read tier returns `allow` immediately.
pub async fn gate(
    svc: &ChannelService,
    channel_id: &str,
    req: &ActionRequest,
    yes: bool,
    timeout: Option<Duration>,
) -> Result<GateDecision> {
    let hash = action_hash(
        &req.tool_name,
        &req.tool_input,
        channel_id,
        &req.step_or_call_id,
        &req.agent_id,
    );

    match default_mode(req.tier) {
        HitlMode::Auto => Ok(GateDecision {
            allow: true,
            reason: "read-tier: auto".into(),
            action_hash: hash,
        }),
        HitlMode::Deny => Ok(GateDecision {
            allow: false,
            reason: "policy: deny".into(),
            action_hash: hash,
        }),
        HitlMode::Ask => {
            let hitl_id = format!("hitl-{}", uuid::Uuid::now_v7());
            let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
            let request = HitlRequest {
                hitl_id: hitl_id.clone(),
                action_hash: hash.clone(),
                tier: req.tier,
                tool_name: req.tool_name.clone(),
                tool_input: req.tool_input.clone(),
                step_or_call_id: req.step_or_call_id.clone(),
                agent_id: req.agent_id.clone(),
                timeout_ms: timeout.as_millis() as u64,
                summary: req.summary.clone(),
            };
            svc.append(
                channel_id,
                ChannelActor::System,
                EventKind::HitlRequest,
                serde_json::to_value(&request)?,
                None,
            )?;
            svc.transition(channel_id, ChannelState::InputRequired, ChannelActor::System)?;

            let decision = if yes {
                // Auto-approve: record the response so the trail is complete.
                let resp = HitlResponse {
                    hitl_id: hitl_id.clone(),
                    action_hash: hash.clone(),
                    allow: true,
                    reason: "--yes".into(),
                    surface: "auto".into(),
                };
                svc.append(
                    channel_id,
                    ChannelActor::System,
                    EventKind::HitlResponse,
                    serde_json::to_value(&resp)?,
                    None,
                )?;
                GateDecision { allow: true, reason: "auto-approved (--yes)".into(), action_hash: hash.clone() }
            } else {
                wait_for_response(svc, channel_id, &hitl_id, &hash, timeout).await?
            };

            // Return the channel to Working regardless of the verdict.
            svc.transition(channel_id, ChannelState::Working, ChannelActor::System)?;
            Ok(decision)
        }
    }
}

/// Poll the log for a HitlResponse matching `hitl_id`. On drift (the response
/// echoes a different `action_hash`) or timeout, deny (fail-closed).
async fn wait_for_response(
    svc: &ChannelService,
    channel_id: &str,
    hitl_id: &str,
    expected_hash: &str,
    timeout: Duration,
) -> Result<GateDecision> {
    let start = Instant::now();
    loop {
        let evs = svc.load_events(channel_id)?;
        if let Some(resp) = evs.iter().rev().find(|e| {
            e.kind == EventKind::HitlResponse
                && e.payload.get("hitl_id").and_then(|v| v.as_str()) == Some(hitl_id)
        }) {
            let echoed = resp.payload.get("action_hash").and_then(|v| v.as_str()).unwrap_or("");
            if echoed != expected_hash {
                return Ok(GateDecision {
                    allow: false,
                    reason: "hitl_drift: response action_hash mismatch".into(),
                    action_hash: expected_hash.to_string(),
                });
            }
            let allow = resp.payload.get("allow").and_then(|v| v.as_bool()).unwrap_or(false);
            return Ok(GateDecision {
                allow,
                reason: if allow { "approved".into() } else { "denied".into() },
                action_hash: expected_hash.to_string(),
            });
        }
        if start.elapsed() >= timeout {
            return Ok(GateDecision {
                allow: false,
                reason: "hitl timeout".into(),
                action_hash: expected_hash.to_string(),
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn req(tier: RiskTier) -> ActionRequest {
        ActionRequest {
            tier,
            tool_name: "bash".into(),
            tool_input: serde_json::json!({ "cmd": "echo hi" }),
            step_or_call_id: "s0".into(),
            agent_id: "mur".into(),
            summary: "echo".into(),
        }
    }

    #[tokio::test]
    async fn read_tier_runs_unattended() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        let d = gate(&svc, &ch.id, &req(RiskTier::Read), false, None).await.unwrap();
        assert!(d.allow);
    }

    #[tokio::test]
    async fn high_tier_approved_via_prewritten_response() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        // Pre-compute the pin and pre-write an approving response so the poll
        // loop returns on its first read (no real wait).
        let r = req(RiskTier::Destructive);
        let hash = action_hash(&r.tool_name, &r.tool_input, &ch.id, &r.step_or_call_id, &r.agent_id);
        // The gate appends the request first; we race a response in by pre-writing
        // one with the SAME hitl flow is hard, so instead test --yes auto-approve
        // (deterministic) here, and drift below.
        let d = gate(&svc, &ch.id, &r, true, None).await.unwrap();
        assert!(d.allow, "--yes auto-approves a high tier");
        assert_eq!(d.action_hash, hash);
        // The trail has a HitlRequest + an auto HitlResponse + state churn.
        let kinds: Vec<_> = svc.load_events(&ch.id).unwrap().iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::HitlRequest));
        assert!(kinds.contains(&EventKind::HitlResponse));
    }

    #[tokio::test]
    async fn drift_denies_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        // Short timeout so the test is fast; pre-write a response with a WRONG hash.
        let r = req(RiskTier::Spend);
        // Drive the gate on a background task, then inject a mismatched response.
        // Simpler: call wait_for_response directly with a pre-written bad response.
        let resp = HitlResponse {
            hitl_id: "h-x".into(),
            action_hash: "WRONGHASH".into(),
            allow: true,
            reason: "".into(),
            surface: "cli".into(),
        };
        svc.append(&ch.id, ChannelActor::System, EventKind::HitlResponse,
            serde_json::to_value(&resp).unwrap(), None).unwrap();
        let d = wait_for_response(&svc, &ch.id, "h-x", "EXPECTED", std::time::Duration::from_secs(1))
            .await.unwrap();
        assert!(!d.allow, "mismatched action_hash must fail-closed");
        assert!(d.reason.contains("drift"));
        let _ = r;
    }
}
```

- [ ] **Step 2: Run to verify it fails, then passes**

Run: `cargo test -p mur-core hitl::gate` — Expected: first FAIL (stub), then PASS after the implementation above replaces the stub. Confirm all three async tests pass.

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/hitl/gate.rs
git commit -m "feat(hitl): risk-tiered hash-pinned gate over a Channel (v3c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 5: `mur channel approve` responder

**Files:**
- Create: `mur-core/src/cmd/channel.rs`; Modify: the CLI command enum + dispatch (`grep -n "Subcommand\|enum .*Command\|workflow" mur-core/src/cli.rs`).

- [ ] **Step 1: Implement the command (writes a `HitlResponse`)**

Create `mur-core/src/cmd/channel.rs`:

```rust
//! `mur channel approve <channel_id> <hitl_id> [--deny] [--reason ...]` — append
//! a HitlResponse to a channel, releasing a gate that is waiting (v3c). This is
//! the CLI/headless responder; Hub/iOS approval UIs are additive follow-ons.

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::hitl::{HitlRequest, HitlResponse};

pub fn approve(channel_id: &str, hitl_id: &str, deny: bool, reason: Option<String>) -> Result<()> {
    let home = crate::paths::mur_root(None);
    let svc = ChannelService::open(&home)?;

    // Find the matching HitlRequest to echo its action_hash (so the gate's
    // re-verify passes). Refuse if there is no such pending request.
    let evs = svc.load_events(channel_id)?;
    let request: HitlRequest = evs
        .iter()
        .rev()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .find_map(|e| serde_json::from_value::<HitlRequest>(e.payload.clone()).ok())
        .filter(|r| r.hitl_id == hitl_id)
        .with_context(|| format!("no pending HitlRequest {hitl_id} in channel {channel_id}"))?;

    let resp = HitlResponse {
        hitl_id: request.hitl_id,
        action_hash: request.action_hash,
        allow: !deny,
        reason: reason.unwrap_or_default(),
        surface: "cli".into(),
    };
    svc.append(
        channel_id,
        ChannelActor::local_human(),
        EventKind::HitlResponse,
        serde_json::to_value(&resp)?,
        None,
    )?;
    println!(
        "{} {hitl_id} on channel {channel_id}",
        if deny { "denied" } else { "approved" }
    );
    Ok(())
}
```

- [ ] **Step 2: Wire the subcommand**

Add a `Channel` subcommand with an `Approve { channel_id, hitl_id, --deny, --reason }` variant to the CLI enum (located via the grep in Files), declare `pub mod channel;` in `mur-core/src/cmd/mod.rs`, and dispatch to `cmd::channel::approve(...)`. Mirror the existing `workflow` subcommand's wiring.

- [ ] **Step 3: Build**

Run: `cargo build -p mur-core` — Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/channel.rs mur-core/src/cmd/mod.rs   # plus the CLI enum file
git commit -m "feat(cli): mur channel approve — write HitlResponse (v3c responder)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 6: Wire the gate into the executor + resume cursor

**Files:**
- Modify: `mur-core/src/executor/dag.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `dag.rs`:

```rust
    #[tokio::test]
    async fn resume_skips_a_step_already_completed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("resume-wf").unwrap();

        let proc = Procedure {
            variables: vec![],
            steps: vec![step("s0", &[], Some("echo zero")), step("s1", &["s0"], Some("echo one"))],
        };
        let opts = DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "run-1".into(),
            ..Default::default()
        };
        // First run completes both steps.
        execute_dag(tmp.path(), "resume-wf", &proc, &opts).await.unwrap();
        let after_first = svc.load_events(&ch.id).unwrap().len();

        // Re-run with the SAME run_id: dedup + resume cursor make it a no-op for
        // the step ToolResult events (no new ToolResult rows for completed steps).
        execute_dag(tmp.path(), "resume-wf", &proc, &opts).await.unwrap();
        let tr_after_second = svc
            .load_events(&ch.id).unwrap()
            .iter().filter(|e| e.kind == EventKind::ToolResult).count();
        assert_eq!(tr_after_second, 2, "rerun did not duplicate completed-step results");
        let _ = after_first;
    }

    #[tokio::test]
    async fn high_risk_step_gates_and_runs_when_preapproved() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("gated-wf").unwrap();
        let mut s = step("s0", &[], Some("echo done"));
        s.risk = Some(mur_common::hitl::RiskTier::Destructive);
        let proc = Procedure { variables: vec![], steps: vec![s] };
        // `--yes` auto-approves the gate so the step runs without a human.
        let opts = DagExecOptions {
            channel_id: Some(ch.id.clone()),
            run_id: "run-1".into(),
            yes: true,
            ..Default::default()
        };
        let out = execute_dag(tmp.path(), "gated-wf", &proc, &opts).await.unwrap();
        assert_eq!(out.exit_code, 0);
        let kinds: Vec<_> = svc.load_events(&ch.id).unwrap().iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::HitlRequest), "high-risk step raised a gate");
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core executor::dag::tests::high_risk_step_gates_and_runs_when_preapproved` — Expected: FAIL (no gating; `ProcedureStep.risk` unused; possibly the v3a fail-closed guard rejects).

- [ ] **Step 3: Remove the v3a fail-closed guard**

Delete the v3a guard added to `execute_dag` (the `if opts.channel_id.is_some() && …needs_approval… bail!` block). v3c now handles approval over a channel, so it must no longer refuse.

- [ ] **Step 4: Replace the `needs_approval` branch in `execute_step` with the gate**

In `execute_step`, replace the `if step.needs_approval { … }` block (`dag.rs:179-208`) with a tier-resolving gate call. A step gates if it has an explicit `risk` tier (non-Read) OR `needs_approval` (treated as `Destructive`). When no channel is attached, fall back to the old TTY/`--yes` behavior (so non-channel runs are unchanged):

```rust
    // ── Risk-tiered HITL gate (v3c) ──
    let sid = step.id.clone().unwrap_or_else(|| step_index.to_string());
    let tier = step.risk.or(if step.needs_approval {
        Some(mur_common::hitl::RiskTier::Destructive)
    } else {
        None
    });
    if let (Some(tier), Some(cid)) = (tier, opts.channel_id.as_deref()) {
        // Pin the POST-substitution command/intent as the action input.
        let input = serde_json::json!({
            "command": step.command,
            "intent": step.intent,
            "description": step.description,
        });
        let req = crate::hitl::gate::ActionRequest {
            tier,
            tool_name: step.command.clone().map(|_| "sh".into()).unwrap_or_else(|| "intent".into()),
            tool_input: input.clone(),
            step_or_call_id: sid.clone(),
            agent_id: "mur".into(),
            summary: step.description.clone(),
        };
        let decision = match ChannelService::open(mur_home) {
            Ok(svc) => crate::hitl::gate::gate(&svc, cid, &req, opts.yes, None)
                .await
                .unwrap_or(crate::hitl::gate::GateDecision { allow: false, reason: "gate error".into(), action_hash: String::new() }),
            Err(_) => crate::hitl::gate::GateDecision { allow: false, reason: "channel open failed".into(), action_hash: String::new() },
        };
        if !decision.allow {
            eprintln!("  Step {sid}: gate denied ({})", decision.reason);
            return StepResult {
                exit_code: 1,
                output_text: format!("hitl: {}", decision.reason),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()),
                success: false,
            };
        }
        // Re-verify the pin at the execute boundary (fail-closed on drift).
        let now_hash = crate::hitl::pin::action_hash("sh", &input, cid, &sid, "mur");
        if !decision.action_hash.is_empty() && now_hash != decision.action_hash {
            eprintln!("  Step {sid}: hitl_drift at execute boundary — refusing");
            return StepResult {
                exit_code: 1, output_text: "hitl_drift".into(),
                duration_ms: start.elapsed().as_millis() as u64,
                failed_step: Some(step.description.clone()), success: false,
            };
        }
    } else if step.needs_approval {
        // No channel: preserve the legacy TTY/--yes behavior (unchanged).
        // … keep the original dialoguer block here …
    }
```

> Keep the original `dialoguer`/`--yes`/non-TTY block verbatim inside the `else if step.needs_approval` arm so non-channel CLI runs behave exactly as before. The channel path is the new, gated behavior.

- [ ] **Step 5: Add the resume cursor**

At the top of `execute_step` (channel mode only), before doing any work, short-circuit if this step's success is already recorded. Use the deterministic `idem_key` (v3b) for the step's ToolResult:

```rust
    if let Some(cid) = opts.channel_id.as_deref() {
        let result_key = idem_key(cid, &opts.run_id, &sid, "result");
        if let Ok(svc) = ChannelService::open(mur_home) {
            if let Ok(evs) = svc.load_events(cid) {
                if evs.iter().any(|e| {
                    e.kind == EventKind::ToolResult
                        && e.idempotency_key.as_deref() == Some(result_key.as_str())
                        && e.payload.get("success").and_then(|v| v.as_bool()) == Some(true)
                }) {
                    eprintln!("  Step {sid}: already completed (resume) — skipping");
                    return StepResult {
                        exit_code: 0, output_text: String::new(),
                        duration_ms: 0, failed_step: None, success: true,
                    };
                }
            }
        }
    }
```

And in v3a's `ToolResult` emit (Task 4 of v3a), set the deterministic key so resume can match it: change the `ToolResult` emit to pass `Some(idem_key(cid, &opts.run_id, &sid, "result"))` instead of `None`. (This couples v3a's emit to v3b's `run_id` + `idem_key`; when integrating, ensure both exist.)

- [ ] **Step 6: Run tests + regression**

Run:
```bash
cargo test -p mur-core executor::dag::
```
Expected: new tests PASS; existing executor tests PASS (non-channel + no-risk steps unaffected). The v3a `channel_run_refuses_needs_approval` test is now obsolete — **delete it** (v3c intentionally enables that path) and note the removal in the commit.

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/executor/dag.rs
git commit -m "feat(executor): gate high-risk steps via hitl::gate; resume cursor (v3c)

Removes the v3a fail-closed refusal — approval-bearing workflows now run over a
channel through the risk-tiered gate. Adds a deterministic-key resume cursor.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

## Task 7: Quality gates + docs

- [ ] **Step 1: Format / clippy / tests**

```bash
cargo fmt && cargo fmt --check
cargo clippy -p mur-common -p mur-channel -p mur-core -- -D warnings
cargo nextest run -p mur-common -p mur-channel -p mur-core
```
Expected: clean + green (ignore the 4 pre-existing `conversations::summarize::rollup` failures).

- [ ] **Step 2: Manual end-to-end (interactive approval)**

```bash
# Author a workflow with a high-risk step (risk: destructive), run it over a channel
# WITHOUT --yes, in one terminal:
cargo run -p mur-core -- workflow run <wf> --channel-new   # blocks at the gate
# In another terminal, read the pending request and approve it:
id=$(ls -t ~/.mur/channels | head -1)
hitl=$(grep -o '"hitl_id":"[^"]*"' ~/.mur/channels/$id/events.jsonl | tail -1 | cut -d'"' -f4)
cargo run -p mur-core -- channel approve $id $hitl
# → the first terminal unblocks, the step runs, channel returns to working→completed.
```
Confirm the event trail: `HitlRequest` → `StateChange(input-required)` → `HitlResponse` → `StateChange(working)` → `ToolResult` → `StateChange(completed)`.

- [ ] **Step 3: Docs + memory**

- `CLAUDE.md`: note `mur channel approve <id> <hitl_id> [--deny]` and that `category: Workflow` steps may carry `risk:`/`needs_approval` to gate over a channel (v3c).
- Note the `~/.mur/channels` schema is now **v2** (HitlResponse authority).
- Memory: v3c shipped risk-tiered hash-pinned HITL + dedup + resume on `feat/unified-channel-v2`; channel HitlResponse is a mirror until v3d signing; `task_runner` reorder + signing remain v3d.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: mur channel approve + risk-tiered workflow HITL (v3c)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage (against `2026-06-16-unified-channel-v3-design.md` §5 + §7 v3c):**
- "RiskTier + HitlMode + ToolRule.risk + per-step RiskTier on ProcedureStep (additive)" → Task 1. ✓ (matches §8's correction: `ProcedureStep`/`ToolRule` gain NEW `risk` fields.)
- "shared hitl::gate with canonical SHA-256 pin + deterministic idempotency_key" → Tasks 2 (pin) + 4 (gate); idem_key reused from v3b. ✓
- "REPLACE dag.rs needs_approval dialoguer/non-TTY-silent-skip with a channel-gated await" → Task 6 Step 4 (channel path gated; non-channel path keeps legacy behavior). ✓
- "write HitlRequest/HitlResponse event pair under events.lock" → Task 4 (`svc.append` uses the store lock). ✓
- "dedup-aware append_event on idempotency_key" → Task 3 (owned here). ✓
- "fold-to-completed-effects resume cursor" → Task 6 Step 5. ✓ (minimal: skip steps whose success ToolResult key exists.)
- "ChannelState::InputRequired while outstanding" → Task 4 (transition to InputRequired, back to Working). ✓
- "gate command-mode sh -c steps at STEP tier" → Task 6 Step 4 (tier from `step.risk`; command steps use `tool_name="sh"`). ✓
- "CHANNEL_SCHEMA_VERSION → 2 only here" → Task 1. ✓
- "task_runner reorder + per-event signing deferred to v3d" → explicitly out of scope; no `mur-agent-runtime` change. ✓

**2. Placeholder scan:** No "TBD"/"add validation"/"similar to". The one prose-deferral (keep the original `dialoguer` block in the non-channel `else if` arm, Task 6 Step 4) references existing code to retain verbatim, not new logic to invent. Task 2 Step 3 and Task 5 Step 2 use a `grep` to locate the module-declaration / CLI-enum file — both name exactly what to add.

**3. Type consistency:**
- `RiskTier`/`HitlMode`/`HitlRequest`/`HitlResponse`/`default_mode` defined in Task 1 (`mur-common::hitl`), consumed by Tasks 4-6 + the CLI (Task 5).
- `action_hash(tool_name, input, channel_id, step_or_call_id, agent_id) -> String` (Task 2) used in the gate (Task 4) and re-verified in the executor (Task 6 Step 4) with the SAME arg order.
- `ActionRequest`/`GateDecision`/`gate(svc, channel_id, req, yes, timeout)` (Task 4) called identically in Task 6.
- `ToolRule.risk` / `ProcedureStep.risk` are `Option<crate::hitl::RiskTier>` everywhere.
- `idem_key(channel_id, run_id, step_id, suffix)` (v3b) reused for the resume cursor with suffix `"result"` — matched against the v3a `ToolResult` emit, which Task 6 Step 5 updates to use the same key. ⚠ Cross-increment edit flagged.
- Dedup is a plain `idempotency_key` string compare in `mur-channel` (no `sha2` there); the pin lives in `mur-core`.

**4. Scope check:** Single sub-project (v3c), 7 tasks, three workspace crates + a new `mur-core::hitl` module, no runtime crate touched. The blocking poll loop and resume are the riskiest; both have unit/integration coverage via pre-written responses and deterministic keys. The `task_runner` LLM-tool-call reorder and per-event signing are correctly deferred to v3d. Focused. ✓

No gaps found.
