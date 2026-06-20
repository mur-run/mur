# Commander Governance Hooks — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the fleet loop + daemon honor a signed, replay-resistant commander **kill** and **budget ceiling** posted to the fleet channel, recorded in the audit chain, delivered v1 via a local `mur commander` CLI.

**Architecture:** A directive is a channel event with a typed `commander_directive` payload (`idempotency_key = nonce`), signed by the pinned commander key. `fold_governance` verifies each against accepted pubkeys, dedups nonces, **orders by the signed `issued_at_ms` (not store seq)**, last-wins per kind → `GovernanceState{killed, budget_ceiling}`. The loop reads it at the top of every iteration (fail-closed on read error); kill halts, ceiling tightens budget via `min` (0 = hard halt).

**Tech Stack:** Rust 2024. Reuse `mur-channel::sign::{sign_event, verify_event_sig, sign_input}`, `ChannelService::{open, load_events, append_signed}`, `mur-core::conversations::audit::Audit`, `AgentIdentity`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-06-20-commander-governance-hooks-design.md` (rev-2, commit 93601f3f).
- Brand **MUR**; CLI/paths lowercase. No hardcoded magic values (named consts).
- Trust = signature ✓ against a pinned pubkey **AND** strictly-newer-by-`issued_at_ms`. Signature-valid ≠ fresh.
- Fail-safe = less autonomy: no pinned key → inert; bad/stale directive → ignored; channel **read error → fail-closed** (loop halts `CommanderKilled`; daemon skips launch); `budget_ceiling == 0` → **honored** (halt); `< 0`/NaN → ignored.
- Honest-node model: hooks raise the bar; a local-root user can still evade (delete key/file/audit) — real enforcement is the engine's network side. Do not overclaim in comments/help text.
- `mur-common` types-only (no I/O); verify/fold in `mur-channel`; I/O in `mur-core`/`mur-daemon`. Files ≤ 800 lines.
- **Build/test (disk at 100%, ~7.6 GiB free):** prefix EVERY cargo command with `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download` (shared prebuilt target; a fresh worktree target ENOSPCs). Lint `cargo fmt -p <crate>` + `... cargo clippy -p <crate> --no-deps -- -D warnings`. Worktree `.claude/worktrees/commander-hooks`, branch `feat/commander-hooks`.

---

## File Structure

| File | New/Mod | Responsibility |
|---|---|---|
| `mur-common/src/commander.rs` | New | `CommanderDirective`, `GovernanceState`, `COMMANDER_DIRECTIVE_KEY` — pure types. |
| `mur-common/src/lib.rs` | Mod | `pub mod commander;` |
| `mur-channel/src/governance.rs` | New | `fold_governance` (verify multi-key + nonce-dedup + issued_at order + last-wins). |
| `mur-channel/src/lib.rs` | Mod | `pub mod governance;` |
| `mur-core/src/cmd/commander.rs` | New | `accepted_pubkeys`, `governance_state`, `cmd_commander_{pin,status,directive}`. |
| `mur-core/src/cmd/mod.rs` | Mod | `pub mod commander;` |
| `mur-core/src/conversations/audit.rs` | Mod | `AuditAction::Governance`. |
| `mur-core/src/cmd/fleet/loop_run.rs` | Mod | `LoopStop::CommanderKilled` + top-of-iteration governance + budget ceiling + audit. |
| `mur-daemon/src/fleet_tick.rs` | Mod | `due_fleets` skips commander-killed (Err → fail-closed skip). |
| `mur-core/src/cli/mod.rs` + `dispatch.rs` | Mod | `mur commander` subcommand. |

---

## Task 1: Commander payload types (`mur-common`)

**Files:** Create `mur-common/src/commander.rs`; Modify `mur-common/src/lib.rs`. Test: inline.

**Interfaces — Produces:**
- `pub const COMMANDER_DIRECTIVE_KEY: &str = "commander_directive";`
- `pub struct CommanderDirective { pub kind: String, pub fleet: String, pub budget_usd: Option<f64>, pub nonce: String, pub issued_at_ms: u64 }` (serde Serialize/Deserialize)
- `#[derive(Default)] pub struct GovernanceState { pub killed: bool, pub budget_ceiling: Option<f64> }`

- [ ] **Step 1: Write the failing test** — create `mur-common/src/commander.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directive_roundtrips_under_the_marker_key() {
        let d = CommanderDirective {
            kind: "kill".into(), fleet: "dev".into(), budget_usd: None,
            nonce: "n1".into(), issued_at_ms: 1_750_000_000_000,
        };
        let wrapped = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: &d });
        let got: CommanderDirective =
            serde_json::from_value(wrapped[COMMANDER_DIRECTIVE_KEY].clone()).unwrap();
        assert_eq!(got.kind, "kill");
        assert_eq!(got.fleet, "dev");
        assert_eq!(got.issued_at_ms, 1_750_000_000_000);
    }
    #[test]
    fn governance_state_default_is_inert() {
        let g = GovernanceState::default();
        assert!(!g.killed && g.budget_ceiling.is_none());
    }
}
```

- [ ] **Step 2: Run → fail**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-common -E 'test(/commander/)'`
Expected: FAIL (types undefined).

- [ ] **Step 3: Implement** — prepend to `mur-common/src/commander.rs`:

```rust
//! Commander governance directive types (pure; no I/O). A directive rides in a
//! channel event's payload under `COMMANDER_DIRECTIVE_KEY`, signed by the
//! commander key; `mur-channel::governance::fold_governance` verifies + folds.

use serde::{Deserialize, Serialize};

/// Payload key carrying a `CommanderDirective` inside a channel event's payload.
pub const COMMANDER_DIRECTIVE_KEY: &str = "commander_directive";

/// A signed commander directive. `nonce` is also set as the event's
/// `idempotency_key` (binds it into the signature + enables store dedup).
/// `issued_at_ms` is the commander wall-clock; it — NOT the store-assigned seq —
/// is the authoritative ordering/freshness key (replay resistance).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommanderDirective {
    /// "kill" | "resume" | "budget_ceiling".
    pub kind: String,
    /// Target fleet name; must equal the channel's fleet.
    pub fleet: String,
    /// Present only for "budget_ceiling"; >=0 honored, <0/NaN ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// Unique per directive (also the event idempotency_key).
    pub nonce: String,
    /// Commander wall-clock millis — the ordering/freshness key.
    pub issued_at_ms: u64,
}

/// Folded governance state for a fleet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GovernanceState {
    pub killed: bool,
    pub budget_ceiling: Option<f64>,
}
```

Add `pub mod commander;` to `mur-common/src/lib.rs` (near `pub mod channel;`).

- [ ] **Step 4: Run → pass**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-common -E 'test(/commander/)'`
Expected: PASS (2 tests).

- [ ] **Step 5: Lint + commit**
```bash
cargo fmt -p mur-common && CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo clippy -p mur-common --no-deps -- -D warnings
git add mur-common/src/commander.rs mur-common/src/lib.rs
git commit -m "feat(commander): pure directive + governance-state types"
```

---

## Task 2: Replay-resistant governance fold (`mur-channel`) — SECURITY CRUX

**Files:** Create `mur-channel/src/governance.rs`; Modify `mur-channel/src/lib.rs`. Test: inline.

**Interfaces:**
- Consumes (Task 1): `mur_common::commander::{CommanderDirective, GovernanceState, COMMANDER_DIRECTIVE_KEY}`; `mur_common::channel::ChannelEvent`; `crate::sign::verify_event_sig`.
- Produces: `pub fn fold_governance(events: &[ChannelEvent], channel_id: &str, fleet_name: &str, accepted_pubkeys: &[[u8; 32]]) -> GovernanceState`

- [ ] **Step 1: Write the failing test** — create `mur-channel/src/governance.rs` with the test module. (Helper builds a SIGNED directive event the way the store would.)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
    use mur_common::identity::AgentIdentity;

    const CID: &str = "fleet-dev";

    // Build a signed directive event (mirrors what append_signed produces).
    fn directive_event(
        id: &AgentIdentity, seq: u64, kind: &str, fleet: &str,
        budget: Option<f64>, nonce: &str, issued_at_ms: u64,
    ) -> ChannelEvent {
        let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
            "kind": kind, "fleet": fleet, "budget_usd": budget,
            "nonce": nonce, "issued_at_ms": issued_at_ms,
        }});
        let actor = ChannelActor::System;
        let sig = crate::sign::sign_event(id, CID, &actor, EventKind::Note, &payload, Some(nonce));
        ChannelEvent {
            seq, ts: chrono::Utc::now(), actor, kind: EventKind::Note, payload,
            idempotency_key: Some(nonce.to_string()), sig: Some(sig), key_version: None,
        }
    }

    #[test]
    fn kill_then_newer_resume_then_replayed_old_resume_stays_killed() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let resume_old = directive_event(&cmd, 1, "resume", "dev", None, "r1", 1000);
        let kill = directive_event(&cmd, 2, "kill", "dev", None, "k1", 2000);
        // attacker re-appends the OLD resume as a new, higher-seq row (verbatim):
        let mut resume_replay = resume_old.clone();
        resume_replay.seq = 3;
        let evs = vec![resume_old, kill, resume_replay];
        // Despite the replay being last by seq, issued_at order + nonce-dedup keep the kill.
        assert!(fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn newer_resume_clears_a_kill() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let evs = vec![
            directive_event(&cmd, 1, "kill", "dev", None, "k1", 1000),
            directive_event(&cmd, 2, "resume", "dev", None, "r1", 2000),
        ];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn wrong_key_directive_is_ignored() {
        let cmd = AgentIdentity::generate();
        let attacker = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        // kill signed by attacker's key → not a candidate → not killed.
        let evs = vec![directive_event(&attacker, 1, "kill", "dev", None, "k1", 1000)];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn wrong_fleet_directive_is_ignored() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        let evs = vec![directive_event(&cmd, 1, "kill", "other", None, "k1", 1000)];
        assert!(!fold_governance(&evs, CID, "dev", &pk).killed);
    }

    #[test]
    fn budget_ceiling_applied_zero_honored_negative_ignored() {
        let cmd = AgentIdentity::generate();
        let pk = [cmd.verifying_key_bytes()];
        assert_eq!(
            fold_governance(&[directive_event(&cmd, 1, "budget_ceiling", "dev", Some(5.0), "b1", 1000)], CID, "dev", &pk).budget_ceiling,
            Some(5.0)
        );
        assert_eq!(
            fold_governance(&[directive_event(&cmd, 1, "budget_ceiling", "dev", Some(0.0), "b1", 1000)], CID, "dev", &pk).budget_ceiling,
            Some(0.0)
        );
        assert_eq!(
            fold_governance(&[directive_event(&cmd, 1, "budget_ceiling", "dev", Some(-1.0), "b1", 1000)], CID, "dev", &pk).budget_ceiling,
            None
        );
    }

    #[test]
    fn previous_key_still_verifies() {
        let prev = AgentIdentity::generate();
        let cur = AgentIdentity::generate();
        let accepted = [cur.verifying_key_bytes(), prev.verifying_key_bytes()];
        let evs = vec![directive_event(&prev, 1, "kill", "dev", None, "k1", 1000)];
        assert!(fold_governance(&evs, CID, "dev", &accepted).killed);
    }
}
```

- [ ] **Step 2: Run → fail**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-channel -E 'test(/governance/)'`
Expected: FAIL (`fold_governance` undefined).

- [ ] **Step 3: Implement** — prepend to `mur-channel/src/governance.rs`:

```rust
//! Replay-resistant fold of commander governance directives from a channel log.
//!
//! Trust = signature verifies against an accepted pubkey AND the directive is
//! ordered by its SIGNED `issued_at_ms` (NOT the store-assigned `seq`). A
//! replayed old directive keeps its old timestamp and cannot supersede a newer
//! one; verbatim replays are dropped by nonce-dedup. See the design spec §4.

use std::collections::HashSet;

use mur_common::channel::ChannelEvent;
use mur_common::commander::{COMMANDER_DIRECTIVE_KEY, CommanderDirective, GovernanceState};

use crate::sign::verify_event_sig;

/// Fold the channel's commander directives into governance state.
pub fn fold_governance(
    events: &[ChannelEvent],
    channel_id: &str,
    fleet_name: &str,
    accepted_pubkeys: &[[u8; 32]],
) -> GovernanceState {
    // 1. Candidate filter: parse the marker, match the fleet, verify the sig
    //    against SOME accepted pubkey.
    let mut candidates: Vec<CommanderDirective> = Vec::new();
    for e in events {
        let Some(raw) = e.payload.get(COMMANDER_DIRECTIVE_KEY) else {
            continue;
        };
        let Ok(d) = serde_json::from_value::<CommanderDirective>(raw.clone()) else {
            continue;
        };
        if d.fleet != fleet_name {
            continue;
        }
        let Some(sig) = e.sig.as_deref() else {
            continue;
        };
        let ok = accepted_pubkeys.iter().any(|pk| {
            verify_event_sig(
                channel_id,
                &e.actor,
                e.kind,
                &e.payload,
                e.idempotency_key.as_deref(),
                sig,
                pk,
            )
        });
        if ok {
            candidates.push(d);
        }
    }

    // 2. Nonce-dedup (drop verbatim replays; keep first occurrence).
    let mut seen: HashSet<&str> = HashSet::new();
    candidates.retain(|d| seen.insert(d.nonce.as_str()));

    // 3. Order by the SIGNED issued_at_ms (then nonce as a deterministic tiebreak).
    candidates.sort_by(|a, b| {
        a.issued_at_ms
            .cmp(&b.issued_at_ms)
            .then_with(|| a.nonce.cmp(&b.nonce))
    });

    // 4. Last-wins per kind in that order.
    let mut state = GovernanceState::default();
    for d in &candidates {
        match d.kind.as_str() {
            "kill" => state.killed = true,
            "resume" => state.killed = false,
            "budget_ceiling" => {
                if let Some(v) = d.budget_usd
                    && v.is_finite()
                    && v >= 0.0
                {
                    state.budget_ceiling = Some(v);
                }
            }
            _ => {}
        }
    }
    state
}
```

Add `pub mod governance;` to `mur-channel/src/lib.rs`.

- [ ] **Step 4: Run → pass**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-channel -E 'test(/governance/)'`
Expected: PASS (6 tests).

- [ ] **Step 5: Lint + commit**
```bash
cargo fmt -p mur-channel && CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo clippy -p mur-channel --no-deps -- -D warnings
git add mur-channel/src/governance.rs mur-channel/src/lib.rs
git commit -m "feat(commander): replay-resistant governance fold (issued_at order, not seq)"
```

---

## Task 3: `mur commander` CLI + governance wrapper (`mur-core`)

**Files:** Create `mur-core/src/cmd/commander.rs`; Modify `mur-core/src/cmd/mod.rs`, `mur-core/src/cli/mod.rs`, `mur-core/src/dispatch.rs`. Test: inline + integration in the same file.

**Interfaces:**
- Consumes: Task 1 types, Task 2 `mur_channel::governance::fold_governance`; `mur_channel::ChannelService::{open, load_events, append_signed}`; `AgentIdentity::{load, public_key_multibase}`; `ChannelActor::System`, `EventKind::Note`.
- Produces:
  - `pub fn accepted_pubkeys(mur_home: &Path) -> Vec<[u8; 32]>` (current `identity.pub` + optional `identity.prev.pub`; empty = ungoverned)
  - `pub fn governance_state(mur_home: &Path, fleet: &str) -> anyhow::Result<GovernanceState>` (Err on channel read failure)
  - `pub fn cmd_commander_pin(mur_home: &Path, pubkey_multibase: &str, force: bool) -> anyhow::Result<()>`
  - `pub fn cmd_commander_status(mur_home: &Path) -> anyhow::Result<()>`
  - `pub fn cmd_commander_directive(mur_home: &Path, fleet: &str, kind: &str, budget_usd: Option<f64>, now_ms: u64) -> anyhow::Result<()>`
  - const `COMMANDER_DIR: &str = "commander"`, `COMMANDER_PUB: &str = "identity.pub"`, `COMMANDER_PREV_PUB: &str = "identity.prev.pub"`

- [ ] **Step 1: Write the failing test** — create `mur-core/src/cmd/commander.rs` with tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;

    fn seed_commander(home: &std::path::Path) -> AgentIdentity {
        let dir = home.join("commander");
        std::fs::create_dir_all(&dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&dir).unwrap(); // writes identity.key + identity.pub
        id
    }

    #[test]
    fn pin_refuses_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        cmd_commander_pin(home, "z11111", false).unwrap();
        assert!(cmd_commander_pin(home, "z22222", false).is_err());
        cmd_commander_pin(home, "z22222", true).unwrap(); // force overwrites
        let pinned = std::fs::read_to_string(home.join("commander").join(COMMANDER_PUB)).unwrap();
        assert_eq!(pinned.trim(), "z22222");
    }

    #[test]
    fn directive_then_governance_state_reflects_kill() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let id = seed_commander(home); // commander identity (key+pub) present
        // a fleet + its channel must exist to append into
        let fleet = mur_common::fleet::Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(), router: None,
            members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let svc = mur_channel::ChannelService::open(home).unwrap();
        svc.create_for_fleet("dev", "mur", &["pm".into()]).unwrap();

        // no directive yet → not killed
        assert!(!governance_state(home, "dev").unwrap().killed);
        // issue a kill via the CLI path
        cmd_commander_directive(home, "dev", "kill", None, 1000).unwrap();
        assert!(governance_state(home, "dev").unwrap().killed);
        // the pinned pubkey accepts the commander's own key
        assert_eq!(accepted_pubkeys(home), vec![id.verifying_key_bytes()]);
    }
}
```

- [ ] **Step 2: Run → fail**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/commander/)'`
Expected: FAIL (undefined fns).

- [ ] **Step 3: Implement** — prepend to `mur-core/src/cmd/commander.rs`:

```rust
//! `mur commander` — pin the commander key + (v1) issue signed directives into a
//! fleet channel, and fold them into governance state. Engine is the closed crate.

use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::commander::{COMMANDER_DIRECTIVE_KEY, GovernanceState};
use mur_common::identity::AgentIdentity;

pub const COMMANDER_DIR: &str = "commander";
pub const COMMANDER_PUB: &str = "identity.pub";
pub const COMMANDER_PREV_PUB: &str = "identity.prev.pub";

fn decode_pub(multibase: &str) -> Option<[u8; 32]> {
    let (_, bytes) = multibase::decode(multibase.trim()).ok()?;
    bytes.try_into().ok()
}

/// Accepted commander pubkeys: current `identity.pub` + optional previous. Empty
/// vec ⇒ no commander configured (governance inert).
pub fn accepted_pubkeys(mur_home: &Path) -> Vec<[u8; 32]> {
    let dir = mur_home.join(COMMANDER_DIR);
    let mut out = Vec::new();
    for name in [COMMANDER_PUB, COMMANDER_PREV_PUB] {
        if let Ok(s) = std::fs::read_to_string(dir.join(name))
            && let Some(pk) = decode_pub(&s)
        {
            out.push(pk);
        }
    }
    out
}

/// Fold governance for `fleet` from its channel. Err on channel read failure
/// (callers fail-closed). No pinned key ⇒ inert default.
pub fn governance_state(mur_home: &Path, fleet: &str) -> Result<GovernanceState> {
    let keys = accepted_pubkeys(mur_home);
    if keys.is_empty() {
        return Ok(GovernanceState::default());
    }
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let channel_id = format!("fleet-{fleet}");
    let events = svc
        .load_events(&channel_id)
        .with_context(|| format!("load channel {channel_id}"))?;
    Ok(mur_channel::governance::fold_governance(
        &events, &channel_id, fleet, &keys,
    ))
}

pub fn cmd_commander_pin(mur_home: &Path, pubkey_multibase: &str, force: bool) -> Result<()> {
    if decode_pub(pubkey_multibase).is_none() {
        bail!("not a valid multibase Ed25519 pubkey (expected 32 bytes)");
    }
    let dir = mur_home.join(COMMANDER_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(COMMANDER_PUB);
    if path.exists() && !force {
        bail!(
            "a commander key is already pinned at {} — re-pin is a governance change; pass --force",
            path.display()
        );
    }
    std::fs::write(&path, format!("{}\n", pubkey_multibase.trim()))?;
    println!("Pinned commander key → {}", path.display());
    Ok(())
}

pub fn cmd_commander_status(mur_home: &Path) -> Result<()> {
    let keys = accepted_pubkeys(mur_home);
    if keys.is_empty() {
        println!("No commander key pinned (governance inert).");
        return Ok(());
    }
    let dir = mur_home.join(COMMANDER_DIR);
    let cur = std::fs::read_to_string(dir.join(COMMANDER_PUB)).unwrap_or_default();
    println!("Commander key pinned: {}", cur.trim());
    if dir.join(COMMANDER_PREV_PUB).exists() {
        println!("  (previous key also accepted for rotation)");
    }
    Ok(())
}

/// v1 delivery: sign a directive with the local commander identity and append it
/// to the fleet channel. `now_ms` is injected for deterministic tests.
pub fn cmd_commander_directive(
    mur_home: &Path,
    fleet: &str,
    kind: &str,
    budget_usd: Option<f64>,
    now_ms: u64,
) -> Result<()> {
    if !matches!(kind, "kill" | "resume" | "budget_ceiling") {
        bail!("kind must be kill | resume | budget_ceiling");
    }
    let id = AgentIdentity::load(&mur_home.join(COMMANDER_DIR))
        .context("load commander identity (~/.mur/commander/identity.key)")?;
    let nonce = uuid::Uuid::now_v7().to_string();
    let payload = serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
        "kind": kind, "fleet": fleet, "budget_usd": budget_usd,
        "nonce": nonce, "issued_at_ms": now_ms,
    }});
    let svc = mur_channel::ChannelService::open(mur_home)?;
    let ev = svc.append_signed(
        &format!("fleet-{fleet}"),
        &id,
        0,
        mur_common::channel::ChannelActor::System,
        mur_common::channel::EventKind::Note,
        payload,
        Some(nonce.clone()),
    )?;
    println!("Issued commander '{kind}' for fleet '{fleet}' (seq {}, nonce {nonce})", ev.seq);
    Ok(())
}
```

Add `pub mod commander;` to `mur-core/src/cmd/mod.rs`.

- [ ] **Step 4: Wire the CLI** — in `mur-core/src/cli/mod.rs` add to the top-level `Commands` enum (mirroring `Fleet`):

```rust
    /// Commander governance: pin the operator key + issue/inspect directives
    Commander {
        #[command(subcommand)]
        action: CommanderAction,
    },
```

and define (near `FleetAction`, or in `cli/actions.rs` if that's where subaction enums live — follow the existing location):

```rust
#[derive(clap::Subcommand)]
pub enum CommanderAction {
    /// Pin the commander public key (multibase). Refuses overwrite without --force.
    Pin { pubkey: String, #[arg(long)] force: bool },
    /// Show whether a commander key is pinned.
    Status,
    /// Issue a signed directive into a fleet channel (v1 local delivery).
    Directive {
        fleet: String,
        /// kill | resume | budget-ceiling
        kind: String,
        #[arg(long)]
        budget_usd: Option<f64>,
    },
}
```

In `mur-core/src/dispatch.rs` add the arm (mirroring `Commands::Fleet`):

```rust
        Commands::Commander { action } => {
            let mur_home = crate::paths::mur_root(None);
            match action {
                CommanderAction::Pin { pubkey, force } => {
                    cmd::commander::cmd_commander_pin(&mur_home, &pubkey, force)?
                }
                CommanderAction::Status => cmd::commander::cmd_commander_status(&mur_home)?,
                CommanderAction::Directive { fleet, kind, budget_usd } => {
                    // CLI uses "budget-ceiling"; map to the internal "budget_ceiling".
                    let k = if kind == "budget-ceiling" { "budget_ceiling" } else { &kind };
                    let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
                    cmd::commander::cmd_commander_directive(&mur_home, &fleet, k, budget_usd, now_ms)?
                }
            }
        }
```

Ensure `CommanderAction` is imported in `dispatch.rs` the same way `FleetAction` is.

- [ ] **Step 5: Run → pass**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/commander/)'`
Expected: PASS (2 tests).

- [ ] **Step 6: Lint + commit**
```bash
cargo fmt -p mur-core && CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings
git add mur-core/src/cmd/commander.rs mur-core/src/cmd/mod.rs mur-core/src/cli/mod.rs mur-core/src/dispatch.rs mur-core/src/cli/actions.rs
git commit -m "feat(commander): mur commander pin/status/directive + governance_state wrapper"
```

---

## Task 4: Loop governance hook + audit (`mur-core`)

**Files:** Modify `mur-core/src/conversations/audit.rs` (add `Governance`), `mur-core/src/cmd/fleet/loop_run.rs`. Test: inline in `loop_run.rs`.

**Interfaces:**
- Consumes: Task 3 `cmd::commander::{accepted_pubkeys, governance_state}`; `mur_channel::governance::fold_governance`; `Audit::{open, append}`; `mur_channel::sign::sign_input`.
- Produces: `LoopStop::CommanderKilled`; `AuditAction::Governance { fleet, directive, decision, nonce }`.

- [ ] **Step 1: Add the audit variant** — in `mur-core/src/conversations/audit.rs`, add to `AuditAction` (serde tag = "governance"):

```rust
    /// A commander governance directive was honored by a fleet loop.
    Governance {
        fleet: String,
        directive: String, // "kill" | "budget_ceiling"
        decision: String,  // "halted" | "capped"
        nonce: String,
    },
```

- [ ] **Step 2: Write the failing test** — add to `loop_run.rs` tests (uses the Task-3 CLI to plant a kill, then runs one loop iteration against a stub-less path — assert it halts CommanderKilled without needing live members):

```rust
    #[tokio::test]
    async fn commander_kill_halts_loop_and_local_start_cannot_clear_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // commander identity + pinned key
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate().save(&cdir).unwrap();
        // a fleet + channel
        let fleet = Fleet {
            name: "dev".into(), display_name: String::new(), goal: "g".into(), router: None,
            members: vec!["pm".into()], channel_id: "fleet-dev".into(),
            rules: vec![], skills: vec![], loop_cfg: None,
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        mur_channel::ChannelService::open(home).unwrap()
            .create_for_fleet("dev", "mur", &["pm".into()]).unwrap();
        // plant a commander kill
        crate::cmd::commander::cmd_commander_directive(home, "dev", "kill", None, 1000).unwrap();

        // one guarded run: must stop CommanderKilled before doing any work
        let stop = run_loop_for_test(home, "dev").await;
        assert_eq!(stop, LoopStop::CommanderKilled);

        // local kill-switch clear does NOT lift the commander kill
        crate::cmd::fleet::control::cmd_fleet_start(home, "dev").ok();
        let stop2 = run_loop_for_test(home, "dev").await;
        assert_eq!(stop2, LoopStop::CommanderKilled);

        // an audit Governance entry was recorded
        let audit = std::fs::read_to_string(home.join("conversations").join("audit.jsonl")).unwrap();
        assert!(audit.contains("\"kind\":\"governance\"") && audit.contains("\"decision\":\"halted\""));
    }
```

This requires a tiny test seam `run_loop_for_test(mur_home, name) -> LoopStop` that runs the guarded loop body with `max_iterations = 1` against `MUR_HOME` and returns the `LoopStop` (extract the loop's stop-resolution into a helper the public `cmd_fleet_run_loop` calls, OR set `max_iterations: Some(1)` and capture the returned stop). Implementer: refactor `cmd_fleet_run_loop` so the stop reason is produced by an inner `async fn run_guarded(...) -> Result<LoopStop>` that `cmd_fleet_run_loop` prints; the test calls `run_guarded`. Keep behavior identical.

- [ ] **Step 3: Run → fail**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/commander_kill_halts/)'`
Expected: FAIL (`CommanderKilled` undefined / no governance check).

- [ ] **Step 4: Implement** — (a) add the LoopStop variant:

```rust
    /// A commander governance kill (or zero budget-ceiling) halted the loop.
    CommanderKilled,
```

(b) Before the `loop {`, load the accepted keys once:

```rust
    let commander_keys = crate::cmd::commander::accepted_pubkeys(mur_home);
    let governed = !commander_keys.is_empty();
```

(c) At the **very top** of the loop body (before the `is_stopped` check), add the governance read (fail-closed) + ceiling resolution:

```rust
        // Commander governance (highest priority). Fail-closed: a channel read
        // error halts rather than running ungoverned.
        let mut commander_ceiling: Option<f64> = None;
        if governed {
            let gov = match svc.load_events(&fleet.channel_id) {
                Ok(events) => mur_channel::governance::fold_governance(
                    &events, &fleet.channel_id, name, &commander_keys,
                ),
                Err(_) => {
                    emit_governance_audit(mur_home, name, "kill", "halted", "");
                    break LoopStop::CommanderKilled;
                }
            };
            if gov.killed {
                emit_governance_audit(mur_home, name, "kill", "halted", "");
                break LoopStop::CommanderKilled;
            }
            // budget_ceiling == 0 is a hard halt (existing budget_exceeded guards b>0.0).
            if matches!(gov.budget_ceiling, Some(c) if c == 0.0) {
                emit_governance_audit(mur_home, name, "budget_ceiling", "capped", "");
                break LoopStop::CommanderKilled;
            }
            commander_ceiling = gov.budget_ceiling;
        }
```

(d) Tighten the budget at the existing `budget_exceeded` check — combine the local `budget` with `commander_ceiling`:

```rust
        let effective_budget = match (budget, commander_ceiling) {
            (Some(l), Some(c)) => Some(l.min(c)),
            (None, Some(c)) => Some(c),
            (l, None) => l,
        };
        if budget_exceeded(spent, next_cost, effective_budget) {
            break LoopStop::Budget;
        }
```

(e) Add the audit helper near the loop fns:

```rust
/// Record (best-effort) that a commander directive was honored. Never blocks the halt.
fn emit_governance_audit(mur_home: &Path, fleet: &str, directive: &str, decision: &str, nonce: &str) {
    let _ = mur_home; // audit root resolves from MUR_HOME via Audit::open(None)
    if let Ok(audit) = crate::conversations::audit::Audit::open(None) {
        let _ = audit.append(
            crate::conversations::audit::AuditAction::Governance {
                fleet: fleet.to_string(),
                directive: directive.to_string(),
                decision: decision.to_string(),
                nonce: nonce.to_string(),
            },
            String::new(),
        );
    }
}
```

(Note: `content_sha256` is `String::new()` here for the halt record; if the implementer threads the specific directive's `sign_input` hash through `fold_governance`'s result it can be passed instead — optional refinement, not required for the test.)

- [ ] **Step 5: Run → pass**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-core -E 'test(/commander_kill_halts/) + test(/budget/) + test(/is_converged/)'`
Expected: PASS (no regressions in existing budget/convergence tests).

- [ ] **Step 6: Lint + commit**
```bash
cargo fmt -p mur-core && CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo clippy -p mur-core --no-deps -- -D warnings
git add mur-core/src/conversations/audit.rs mur-core/src/cmd/fleet/loop_run.rs
git commit -m "feat(commander): fleet loop honors kill + budget-ceiling, emits governance audit"
```

---

## Task 5: Daemon auto-run skips commander-killed fleets

**Files:** Modify `mur-daemon/src/fleet_tick.rs`. Test: inline.

**Interfaces:** Consumes Task 3 `mur_core::cmd::commander::governance_state`.

- [ ] **Step 1: Write the failing test** — in `fleet_tick.rs` tests, extend the due-fleets fixture: a budgeted, due, interval fleet with a commander kill in its channel must be excluded from `due_fleets`. (Mirror the existing `due_fleets` test setup; add a commander identity + pin + `cmd_commander_directive(... "kill" ...)` then assert the fleet is NOT returned.)

```rust
    #[test]
    fn due_fleets_skips_commander_killed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // commander identity + a due interval fleet with a budget + channel
        let cdir = home.join("commander");
        std::fs::create_dir_all(&cdir).unwrap();
        mur_common::identity::AgentIdentity::generate().save(&cdir).unwrap();
        let mut f = loop_fleet("killed", "interval:1m");
        f.channel_id = "fleet-killed".into();
        store::save_fleet(home, &f).unwrap();
        mur_channel::ChannelService::open(home).unwrap()
            .create_for_fleet("killed", "mur", &[]).unwrap();
        mur_core::cmd::commander::cmd_commander_directive(home, "killed", "kill", None, 1000).unwrap();
        // even though due + budgeted, a commander kill excludes it
        assert!(!due_fleets(home, 5000).unwrap().contains(&"killed".to_string()));
    }
```

- [ ] **Step 2: Run → fail**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-daemon -E 'test(/due_fleets_skips_commander/)'`
Expected: FAIL (not yet skipping).

- [ ] **Step 3: Implement** — in `due_fleets`, inside the per-fleet loop (after the budget/stopped checks, before pushing the name), add:

```rust
        // Commander governance: don't auto-launch a killed fleet. Err → fail-closed skip.
        match mur_core::cmd::commander::governance_state(mur_home, &name) {
            Ok(g) if g.killed => continue,
            Err(_) => continue,
            _ => {}
        }
```

- [ ] **Step 4: Run → pass**
Run: `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo nextest run -p mur-daemon -E 'test(/due_fleets/)'`
Expected: PASS (existing due_fleets tests + the new one).

- [ ] **Step 5: Update docs + lint + commit** — add one line to `CLAUDE.md`'s fleet section: commander governance hooks (kill + budget-ceiling) via `mur commander`, honored by loop + daemon, audited; engine in the closed crate.
```bash
cargo fmt -p mur-daemon && CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target ORT_STRATEGY=download cargo clippy -p mur-daemon --no-deps -- -D warnings
git add mur-daemon/src/fleet_tick.rs CLAUDE.md
git commit -m "feat(commander): daemon auto-run skips commander-killed fleets (fail-closed)"
```

---

## Self-Review

**1. Spec coverage:** §2 directive model → T1 (types) + T3 (directive CLI sets idem=nonce). §3 trust + pin --force → T3. §4 fold (verify multi-key, nonce-dedup, issued_at order, last-wins, budget>=0) → T2. §5 loop (top-of-iteration read, fail-closed, kill→CommanderKilled) → T4. §6 budget min + 0-halt special case → T4(d)+(c). §7 audit Governance → T4(a)+(e). §8 daemon skip + Err-fail-closed → T5. §9 CLI pin/status/directive → T3. §11 fail-safe → T2 (drop neg/NaN), T4 (read err), T3 (no key → inert). §12 tests → T2/T3/T4/T5. **Gap check:** key-rotation (current+prev) → T3 `accepted_pubkeys` + T2 test `previous_key_still_verifies`. ✓ No uncovered requirement.

**2. Placeholder scan:** The only soft spots are deliberately marked: T4's `run_guarded` test seam (concrete refactor instructed) and the `content_sha256 = String::new()` for the halt audit (explicitly optional refinement, with the reproducible-`sign_input` upgrade named). No "TBD"/"handle errors" placeholders.

**3. Type consistency:** `fold_governance(events, channel_id, fleet_name, accepted_pubkeys: &[[u8;32]]) -> GovernanceState` identical across T2 def + T3/T4 calls. `governance_state(mur_home, fleet) -> Result<GovernanceState>` T3 def ↔ T5 call. `CommanderDirective` fields (kind/fleet/budget_usd/nonce/issued_at_ms) consistent T1↔T2↔T3. `LoopStop::CommanderKilled`, `AuditAction::Governance{fleet,directive,decision,nonce}` consistent T4. CLI `budget-ceiling` ↔ internal `budget_ceiling` mapping handled in dispatch (T3 Step 4). ✓
