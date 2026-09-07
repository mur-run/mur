# Hub agent permissions — P3 chat gate — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-07-hub-agent-permissions-design.md` §P3. Branch `feat/hub-permissions-p3-chat-gate` off `main` (P1+P2 are on main since #1189). One PR, six tasks, each commit builds.

## Goal

A chat turn that needs several approvals asks once, a decision the user already made is not asked again for seven days, and an approval can become an exact-name `allow` rule from the card — with no approve-all, no synthetic result, and no unattended path.

## Architecture

The runtime gates a whole LLM response at once: before executing a response's tool calls it resolves every `Ask` call — settled decision from the agent's decision channel, or one `tool/approval_needed` carrying `calls: [...]` — and hands `handle_tool_call` a pre-made decision. Decisions are recorded as signed `HitlResponse` events in a per-agent channel remembered in `agents/<name>/hitl-channel` (the same shape as the scheduler's `schedule-channel` marker). The pin canonicalisation and TTL move from `mur-core` to `mur-common` so the runtime shares them without depending on `mur-core`. The Hub relays the batch, the card offers *Allow this time* / *Always allow* / *Deny* per call, and computes the "always" rule and the entitlement grant hint from the `PermissionsView` it already has (P1), never from the runtime. murmur expands `calls` into its existing one-request-at-a-time queue.

## Tech stack

Same as P1/P2. `mur-channel` is already a runtime dependency; `sha2` is already in `mur-common`.

## Decisions (read before Task 1)

- **D1 Batch always.** Every notification carries `calls`; the legacy top-level fields (`hitl_id`, `tool_name`, `tool_input`, `step_id`, `timeout_ms`) are the FIRST call, so a client that only knows the old shape still shows one card and its answer still lands. murmur is updated in Task 4 so it shows N cards, not one.
- **D2 Deny still ends the turn.** Today a denied `Ask` call returns `Err(hitl_denied)` and the turn stops. P3 keeps that: a batch where any call is denied stops at that call, after the approved calls before it ran. Changing deny into a tool-result the model can argue with is a separate decision, not a side effect of batching.
- **D3 The store is one remembered channel per agent**, id in `<mur_home>/agents/<name>/hitl-channel`, created with `create_for_agent` on first use, events appended signed with the agent's identity as `Agent{self}`. Only `HitlResponse` events are written (no `HitlRequest`: gate B has no risk tier and inventing one to satisfy the struct would be a lie). Lookup verifies each event with the agent's own pubkey via `mur_channel::sign::verify_one`; an unverifiable event is skipped, never trusted.
- **D4 Chat hash** = `mur_common::hitl::pin::action_hash(tool, input, "", CHAT_GATE_STEP, agent)` with `CHAT_GATE_STEP = "chat"`. The step slot is a constant because a per-call id would make every hash unique and the memory useless; the channel slot is empty because chat turns are not bound to a channel.
- **D5 Settled decisions are consulted only where the policy is `Ask`.** An explicit `ToolRule` (P2 / "always") short-circuits before the store. Spec §3.2 says "a human's explicit no outranks any standing grant"; in gate A the standing grant is a blanket tier grant, in gate B the only standing grant is a rule the user wrote for that exact tool. Reading the channel on every `Allow` call to honour a "this time" denial against a later "always" is a file read per tool call for a case the user created on purpose. Documented here; reviewers may overrule in the plan PR.
- **D6 Projection lives in the Hub UI.** The runtime adds only `action_hash` per call. `alwaysRuleFor` (exact name, never a glob, `null` for spend/dispatch tools) and `grantHintFor` (path outside `filesystem.read`/`write`) are pure TS over `HitlRequest` + `PermissionsView`.
- **D7 `surface`.** `tool/hitl_respond` accepts an optional `surface`; the Hub sends `"hub"`. Absent is recorded as `"unknown"`, not guessed.

## Global Constraints

Copied from the spec and `CLAUDE.md`. Every task includes all of them.

1. **No approve-all.** No control, command, or param approves more than one `hitl_id`. Bulk *deny* is allowed.
2. **No synthetic tool result.** A call without an `allow` decision never executes and never returns a fabricated result; the turn errors with `hitl_denied` (D2).
3. **No auto-approval path.** `decide_without_asking` still denies for `can_approve == false`; no client capability, env var, or config releases a gate. `yes:false` stays unreachable from unattended paths.
4. **`always` = `ToolRule { pattern: <exact tool name>, policy: Allow, risk: None }`** written through `cmd_perm_set_tool` (P2), plus the one-time `allow` for this `hitl_id`. It never bundles an entitlement grant; the grant is a separate control that calls `agent_perm_grant_path`.
5. **Profile writes take effect on restart** (Constraint 4 of the spec). The card says `perm.restartHint` after an *always* or a grant when the agent is running. Nothing relies on a profile write to release the open gate — the `hitl_respond` releases it.
6. **`mur-agent-runtime` never depends on `mur-core`.** Shared code goes down to `mur-common` / `mur-channel`.
7. Single source file ≤ 800 lines for NEW files. `task_runner.rs` (4324) and `supervisor.rs` are pre-existing violations: this plan removes more lines from `task_runner.rs` than it adds (the inline ask block moves to `hitl/batch.rs`) and adds ≤ 6 lines to `supervisor.rs`.
8. Brand name is uppercase **MUR** in every user-visible string.
9. Every new user-visible string lands in both `en.ts` and `zh-TW.ts` in the same commit.
10. Tests never touch the DOM. Rust tests never set `MUR_HOME`; channel tests use a `tempfile::tempdir()` home passed explicitly.
11. Every commit is gated on the real exit code: `set -o pipefail; <cmd> 2>&1 | tail -n 20`.

## Working agreement

```bash
export MUR_WEB_DIST=$HOME/Projects/mur-web/dist   # mur-core lib needs it
export ORT_STRATEGY=download
# runtime + common tests
set -o pipefail; cargo nextest run -p mur-agent-runtime --lib hitl 2>&1 | tail -n 20
set -o pipefail; cargo nextest run -p mur-common --lib hitl 2>&1 | tail -n 20
# mur-core (needs the big stack)
set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib hitl 2>&1 | tail -n 20
# clippy per crate, all targets, -D warnings (the CI gate)
set -o pipefail; cargo clippy -p mur-common -p mur-agent-runtime -p mur-core --all-targets -- -D warnings 2>&1 | tail -n 20
# Hub
cd mur-hub-gui/ui && npm test -- --run 2>&1 | tail -n 20 && npm run build 2>&1 | tail -n 5
cd mur-hub-gui/src-tauri && set -o pipefail; cargo clippy --all-targets -- -D warnings 2>&1 | tail -n 20
```

Commit after every task. Check `git branch --show-current` is `feat/hub-permissions-p3-chat-gate` in its own tool call before each commit.

## File structure

| File | Responsibility |
|---|---|
| `mur-common/src/hitl.rs` (modify) | `pub mod pin` (moved from mur-core), `APPROVAL_TTL_SECS`, `within_approval_ttl` |
| `mur-common/src/hitl/pin.rs` (new, moved) | `action_hash`, `PIN_CANON_VERSION` — byte-identical to the mur-core file |
| `mur-core/src/hitl/mod.rs`, `gate.rs` (modify) | `pub use mur_common::hitl::pin;` re-export; TTL const/fn deleted, calls go to mur-common |
| `mur-core/src/hitl/pin.rs` (delete) | — |
| `mur-common/src/agent.rs` (modify) | `HITL_CHANNEL_FILE` beside `SCHEDULE_CHANNEL_FILE` |
| `mur-agent-runtime/src/hitl/mod.rs` (moved from `hitl.rs`) | `HitlDecision` (+`surface`), `HitlApprovals`, `pub mod store; pub mod batch;` |
| `mur-agent-runtime/src/hitl/store.rs` (new) | `DecisionStore` trait, `ChannelDecisionStore`, `chat_action_hash`, `CHAT_GATE_STEP`, `Settled` |
| `mur-agent-runtime/src/hitl/batch.rs` (new) | `PendingCall`, `gate_batch()` — one notification, N oneshots, one timeout, record every decision |
| `mur-agent-runtime/src/task_runner.rs` (modify) | `decision_store` field + `with_decision_store`; loop calls `gate_batch` then `handle_tool_call(.., decision)`; Ask arm shrinks to "use the decision" |
| `mur-agent-runtime/src/supervisor.rs` (modify) | `HitlRespondHandler` reads `surface`; `with_decision_store(ChannelDecisionStore::new(..))` wired |
| `mur-agent-runtime/src/supervisor_runner.rs` (modify) | pass-through of the store into `build_provider_runner` (it already receives `identity`) |
| `mur-core/src/cmd/agent/cli/stream.rs` (modify) | `HitlRequest::from_params(v) -> Vec<Self>` expanding `calls`; the `on_hitl` closure sends one `StreamMsg::Hitl` per call |
| `mur-hub-gui/src-tauri/src/chat.rs` (modify) | relay adds `calls` and `action_hash` |
| `mur-hub-gui/src-tauri/src/hitl.rs` (modify) | `agent_hitl_respond` sends `surface: "hub"` |
| `mur-hub-gui/ui/src/types.ts` (modify) | `HitlRequest.action_hash?`, `HitlRequest.batch_id?`, `HitlBatchPayload` |
| `mur-hub-gui/ui/src/components/hitlModel.ts` (+ `.test.ts`) (new) | `expandBatch`, `alwaysRuleFor`, `NO_ALWAYS_TOOLS`, `grantHintFor`, `leafToolName` |
| `mur-hub-gui/ui/src/components/HitlCard.tsx` (modify) | *Allow* / *Always allow* / *Deny* (+ reason); grant control; restart hint |
| `mur-hub-gui/ui/src/components/ChatTab.tsx` (modify) | expands the batch; fetches `PermissionsView` once per agent for the cards |
| `mur-hub-gui/ui/src/i18n/en.ts`, `zh-TW.ts` (modify) | `hitl.allowOnce`, `hitl.always`, `hitl.alwaysHint`, `hitl.grant`, `hitl.batchTitle` |
| `mur-hub-gui/ui/src/styles/components/chat.css` (modify) | `.hitl-card__btn--always`, `.hitl-card__grant`, `.hitl-batch` |

---

### Task 1 — `mur-common`: the pin and the TTL move down

**Interfaces.** Produces `mur_common::hitl::pin::action_hash(tool_name, input, channel_id, step_or_call_id, agent_id) -> String`, `mur_common::hitl::pin::PIN_CANON_VERSION`, `mur_common::hitl::APPROVAL_TTL_SECS: i64`, `mur_common::hitl::within_approval_ttl(event_ts, now) -> bool`, `mur_common::agent::HITL_CHANNEL_FILE: &str`. Tasks 2, 3 consume them; mur-core keeps compiling through a re-export.

- [ ] `git mv mur-core/src/hitl/pin.rs mur-common/src/hitl/pin.rs`. The moved file changes in exactly one place: nothing — it uses only `sha2` and `serde_json`, both mur-common deps. Confirm: `grep -n "^use" mur-common/src/hitl/pin.rs` → `use sha2::{Digest, Sha256};`.
- [ ] `mur-common/src/hitl.rs`: after the `use serde::{Deserialize, Serialize};` line add:
  ```rust
  pub mod pin;

  /// Approvals and denials settle a gate for this long. Content staleness is
  /// already handled by the hash pin (any input change = a different hash); the
  /// TTL bounds TIME staleness, so a weeks-old approval cannot release a gate
  /// nobody remembers granting. Shared by gate A (`mur-core::hitl::gate`) and
  /// gate B (`mur-agent-runtime::hitl::store`) — one number, or the two gates
  /// remember for different lengths and the Hub cannot explain why.
  pub const APPROVAL_TTL_SECS: i64 = 7 * 24 * 60 * 60;

  /// Pure TTL predicate — split out so the boundary is testable without
  /// backdating channel events.
  pub fn within_approval_ttl(
      event_ts: chrono::DateTime<chrono::Utc>,
      now: chrono::DateTime<chrono::Utc>,
  ) -> bool {
      (now - event_ts).num_seconds() <= APPROVAL_TTL_SECS
  }
  ```
  `chrono` is already a mur-common dependency (`ChannelEvent.ts`). Because `hitl.rs` now has a submodule, move it: `git mv mur-common/src/hitl.rs mur-common/src/hitl/mod.rs` (Rust 2018+ allows `hitl.rs` + `hitl/pin.rs`, but the sibling crates use the `mod.rs` layout — match them).
- [ ] `mur-common/src/hitl/mod.rs` tests module, append:
  ```rust
  #[test]
  fn ttl_boundary_is_inclusive_at_seven_days() {
      let now = chrono::Utc::now();
      let exactly = now - chrono::Duration::seconds(APPROVAL_TTL_SECS);
      let over = now - chrono::Duration::seconds(APPROVAL_TTL_SECS + 1);
      assert!(within_approval_ttl(exactly, now));
      assert!(!within_approval_ttl(over, now));
  }
  ```
- [ ] `mur-common/src/agent.rs`: directly below `pub const SCHEDULE_CHANNEL_FILE: &str = "schedule-channel";` add:
  ```rust
  /// Marker file in the agent's home naming the channel that records chat-gate
  /// decisions (`HitlResponse` events keyed by `action_hash`). Same shape as
  /// `SCHEDULE_CHANNEL_FILE`: created on first use, replaced if it names a
  /// channel that no longer loads.
  pub const HITL_CHANNEL_FILE: &str = "hitl-channel";
  ```
- [ ] `mur-core/src/hitl/mod.rs` becomes:
  ```rust
  //! Risk-tiered, hash-pinned HITL gate for the channel executor (v3c).
  pub mod gate;
  /// The pin lives in `mur-common` since P3 so the agent runtime can share the
  /// canonicalisation without depending on this crate; re-exported so every
  /// `crate::hitl::pin::action_hash` call site stays valid.
  pub use mur_common::hitl::pin;
  ```
- [ ] `mur-core/src/hitl/gate.rs`: delete the `HITL_APPROVAL_TTL_SECS` const and the `within_approval_ttl` fn (lines 51–64). Add `within_approval_ttl` to the existing `use mur_common::hitl::{...}` import. `grep -n "HITL_APPROVAL_TTL_SECS\|within_approval_ttl" mur-core/src` must show only the import and the call in `scan_prior` plus any test that already referenced the fn (update the test to `mur_common::hitl::APPROVAL_TTL_SECS` if it used the old name).
- [ ] `set -o pipefail; cargo nextest run -p mur-common --lib hitl 2>&1 | tail -n 6` → `… passed` including `ttl_boundary_is_inclusive_at_seven_days`, `hash_is_stable_and_order_independent`, `drift_changes_the_hash`. `set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib hitl 2>&1 | tail -n 6` → all gate tests pass unchanged. `cargo clippy -p mur-common -p mur-core --all-targets -- -D warnings` → 0. `cargo fmt`.
- [ ] Commit: `refactor(hitl): pin canonicalisation and approval TTL move to mur-common`

---

### Task 2 — runtime `hitl/store.rs`: the decision channel

**Interfaces.** Consumes Task 1. Produces:
```rust
pub const CHAT_GATE_STEP: &str = "chat";
pub fn chat_action_hash(tool_name: &str, input: &serde_json::Value, agent: &str) -> String;
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Settled { Allow, Deny }
#[async_trait::async_trait]
pub trait DecisionStore: Send + Sync {
    /// Newest verified `HitlResponse` for `action_hash` inside the TTL, if any.
    async fn lookup(&self, action_hash: &str) -> Option<Settled>;
    /// Append a signed `HitlResponse`. Best-effort: an error is logged, never returned.
    async fn record(&self, resp: mur_common::hitl::HitlResponse);
}
pub struct ChannelDecisionStore { .. }
impl ChannelDecisionStore { pub fn new(mur_home: PathBuf, agent: String, identity: Arc<AgentIdentity>, key_version: u32) -> Self }
```
Task 3 consumes `DecisionStore`, `Settled`, `chat_action_hash`.

- [ ] `git mv mur-agent-runtime/src/hitl.rs mur-agent-runtime/src/hitl/mod.rs`; append to it:
  ```rust
  pub mod batch;
  pub mod store;
  ```
  and change `HitlDecision` to:
  ```rust
  /// Decision returned by the Hub (or any HITL responder) for a pending approval.
  #[derive(Debug, Clone)]
  pub struct HitlDecision {
      pub allow: bool,
      pub reason: Option<String>,
      /// Which surface answered ("hub", "cli", "ios"); `None` when the responder
      /// did not say, recorded as "unknown" — never guessed.
      pub surface: Option<String>,
  }
  ```
  Every existing constructor of `HitlDecision` (grep `HitlDecision {` in `task_runner.rs`, `supervisor.rs`) gains `surface: None`. Create `mur-agent-runtime/src/hitl/batch.rs` as an empty file with `//! P3 batch gate — filled in Task 3.` so the module tree compiles.
- [ ] Create `mur-agent-runtime/src/hitl/store.rs`:
  ```rust
  //! Gate B's memory: settled chat-gate decisions, one remembered channel per
  //! agent. `HitlResponse` events only, signed by the agent's own identity.
  //! Lookup is newest-wins inside `mur_common::hitl::APPROVAL_TTL_SECS`, and an
  //! event the agent's pubkey cannot verify is skipped, never trusted.

  use std::path::{Path, PathBuf};
  use std::sync::Arc;

  use anyhow::{Context, Result};
  use mur_channel::ChannelService;
  use mur_common::agent::HITL_CHANNEL_FILE;
  use mur_common::channel::{ChannelActor, EventKind};
  use mur_common::hitl::{HitlResponse, within_approval_ttl};
  use mur_common::identity::AgentIdentity;

  /// The `step_or_call_id` slot of every chat-gate hash. A per-call id would
  /// make every hash unique and the memory useless; a constant makes "the same
  /// tool with the same input" the unit the user actually decided about.
  pub const CHAT_GATE_STEP: &str = "chat";

  /// Canonical hash of a chat tool call. Same pin as gate A, with the channel
  /// slot empty (chat turns are not bound to a channel) and the step slot fixed.
  pub fn chat_action_hash(tool_name: &str, input: &serde_json::Value, agent: &str) -> String {
      mur_common::hitl::pin::action_hash(tool_name, input, "", CHAT_GATE_STEP, agent)
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Settled {
      Allow,
      Deny,
  }

  #[async_trait::async_trait]
  pub trait DecisionStore: Send + Sync {
      async fn lookup(&self, action_hash: &str) -> Option<Settled>;
      async fn record(&self, resp: HitlResponse);
  }

  pub struct ChannelDecisionStore {
      mur_home: PathBuf,
      agent: String,
      identity: Arc<AgentIdentity>,
      key_version: u32,
  }

  impl ChannelDecisionStore {
      pub fn new(
          mur_home: PathBuf,
          agent: String,
          identity: Arc<AgentIdentity>,
          key_version: u32,
      ) -> Self {
          Self {
              mur_home,
              agent,
              identity,
              key_version,
          }
      }

      /// The channel decisions live in — created once, then remembered in the
      /// agent's home. Mirrors `scheduler::schedule_channel`: a marker naming a
      /// channel that no longer loads is replaced, not treated as an error.
      fn channel_id(&self, svc: &ChannelService) -> Result<String> {
          decision_channel(svc, &self.mur_home, &self.agent)
      }

      fn scan(&self, action_hash: &str) -> Result<Option<Settled>> {
          let svc = ChannelService::open(&self.mur_home)?;
          let channel_id = self.channel_id(&svc)?;
          let events = svc.load_events(&channel_id)?;
          drop(svc);
          let pubkey = self.identity.verifying_key_bytes();
          let now = chrono::Utc::now();
          let mut settled = None;
          for e in &events {
              if e.kind != EventKind::HitlResponse {
                  continue;
              }
              let Ok(r) = serde_json::from_value::<HitlResponse>(e.payload.clone()) else {
                  continue;
              };
              if r.action_hash != action_hash {
                  continue;
              }
              // require_sig = true: this store only ever writes signed events,
              // so an unsigned one is not ours.
              if !mur_channel::sign::verify_one(&channel_id, e, &pubkey, true) {
                  continue;
              }
              if within_approval_ttl(e.ts, now) {
                  settled = Some(if r.allow { Settled::Allow } else { Settled::Deny });
              }
          }
          Ok(settled)
      }

      fn append(&self, resp: &HitlResponse) -> Result<()> {
          let svc = ChannelService::open(&self.mur_home)?;
          let channel_id = self.channel_id(&svc)?;
          svc.append_signed(
              &channel_id,
              &self.identity,
              self.key_version,
              ChannelActor::Agent {
                  id: self.agent.clone(),
              },
              EventKind::HitlResponse,
              serde_json::to_value(resp)?,
              Some(format!("chat-hitl:{}", resp.hitl_id)),
          )?;
          Ok(())
      }
  }

  #[async_trait::async_trait]
  impl DecisionStore for ChannelDecisionStore {
      async fn lookup(&self, action_hash: &str) -> Option<Settled> {
          match self.scan(action_hash) {
              Ok(s) => s,
              Err(e) => {
                  tracing::warn!(error = %e, "chat-gate decision lookup failed; asking");
                  None
              }
          }
      }

      async fn record(&self, resp: HitlResponse) {
          if let Err(e) = self.append(&resp) {
              tracing::warn!(error = %e, hitl_id = %resp.hitl_id, "chat-gate decision not recorded");
          }
      }
  }

  fn decision_channel(svc: &ChannelService, mur_home: &Path, agent: &str) -> Result<String> {
      let marker = mur_home.join("agents").join(agent).join(HITL_CHANNEL_FILE);
      if let Ok(raw) = std::fs::read_to_string(&marker) {
          let id = raw.trim();
          if !id.is_empty() && svc.exists(id) {
              return Ok(id.to_string());
          }
      }
      let ch = svc.create_for_agent(agent)?;
      if let Some(dir) = marker.parent() {
          std::fs::create_dir_all(dir)?;
      }
      std::fs::write(&marker, &ch.id).context("record the hitl channel id")?;
      Ok(ch.id)
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      fn store(home: &Path) -> ChannelDecisionStore {
          ChannelDecisionStore::new(
              home.to_path_buf(),
              "qa".into(),
              Arc::new(AgentIdentity::generate()),
              0,
          )
      }

      fn resp(hash: &str, allow: bool, id: &str) -> HitlResponse {
          HitlResponse {
              hitl_id: id.into(),
              action_hash: hash.into(),
              allow,
              reason: String::new(),
              surface: "hub".into(),
          }
      }

      #[tokio::test]
      async fn unknown_hash_is_none() {
          let tmp = tempfile::tempdir().unwrap();
          assert_eq!(store(tmp.path()).lookup("nope").await, None);
      }

      #[tokio::test]
      async fn recorded_allow_is_found_and_newest_wins() {
          let tmp = tempfile::tempdir().unwrap();
          let s = store(tmp.path());
          s.record(resp("h1", true, "a")).await;
          assert_eq!(s.lookup("h1").await, Some(Settled::Allow));
          s.record(resp("h1", false, "b")).await;
          assert_eq!(s.lookup("h1").await, Some(Settled::Deny), "newest decision counts");
      }

      #[tokio::test]
      async fn marker_survives_and_channel_is_reused() {
          let tmp = tempfile::tempdir().unwrap();
          let s = store(tmp.path());
          s.record(resp("h1", true, "a")).await;
          let marker = tmp.path().join("agents/qa").join(HITL_CHANNEL_FILE);
          let id = std::fs::read_to_string(&marker).unwrap();
          s.record(resp("h2", true, "b")).await;
          assert_eq!(std::fs::read_to_string(&marker).unwrap(), id);
      }

      #[tokio::test]
      async fn expired_decision_is_not_settled() {
          let tmp = tempfile::tempdir().unwrap();
          let s = store(tmp.path());
          s.record(resp("h1", true, "a")).await;
          // Backdate the event on disk: the store reads `ts` from the log.
          let svc = ChannelService::open(tmp.path()).unwrap();
          let id = std::fs::read_to_string(tmp.path().join("agents/qa").join(HITL_CHANNEL_FILE)).unwrap();
          let path = svc.store().events_path(id.trim());
          let raw = std::fs::read_to_string(&path).unwrap();
          let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
          let rewritten: Vec<String> = raw
              .lines()
              .map(|l| {
                  let mut v: serde_json::Value = serde_json::from_str(l).unwrap();
                  v["ts"] = serde_json::Value::String(old.clone());
                  v.to_string()
              })
              .collect();
          std::fs::write(&path, rewritten.join("\n") + "\n").unwrap();
          assert_eq!(s.lookup("h1").await, None);
      }

      #[tokio::test]
      async fn unsigned_event_is_ignored() {
          let tmp = tempfile::tempdir().unwrap();
          let s = store(tmp.path());
          let svc = ChannelService::open(tmp.path()).unwrap();
          let id = decision_channel(&svc, tmp.path(), "qa").unwrap();
          svc.append(
              &id,
              ChannelActor::Agent { id: "qa".into() },
              EventKind::HitlResponse,
              serde_json::to_value(resp("h1", true, "forged")).unwrap(),
              None,
          )
          .unwrap();
          assert_eq!(s.lookup("h1").await, None);
      }

      #[test]
      fn chat_hash_ignores_call_id_and_key_order() {
          let a = chat_action_hash("bash", &serde_json::json!({"b": 1, "a": 2}), "qa");
          let b = chat_action_hash("bash", &serde_json::json!({"a": 2, "b": 1}), "qa");
          assert_eq!(a, b);
          assert_ne!(a, chat_action_hash("bash", &serde_json::json!({"a": 3}), "qa"));
          assert_ne!(a, chat_action_hash("bash", &serde_json::json!({"b": 1, "a": 2}), "pm"));
      }
  }
  ```
  `svc.store().events_path(..)` and `svc.append(..)` signatures: confirm with `grep -n "pub fn events_path\|pub fn append(" mur-channel/src/store.rs mur-channel/src/service.rs` before writing the two tests that use them; if `events_path` does not exist, use `mur_home.join("channels").join(id).join("events.jsonl")` after confirming the layout with `find <tmp> -name '*.jsonl'` in a scratch run. `tempfile` is already a dev-dependency of the runtime (`grep tempfile mur-agent-runtime/Cargo.toml`).
- [ ] `set -o pipefail; cargo nextest run -p mur-agent-runtime --lib hitl::store 2>&1 | tail -n 8` → `6 tests run: 6 passed`. `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings` → 0. `cargo fmt -p mur-agent-runtime`.
- [ ] Commit: `feat(runtime): chat-gate decision store — signed HitlResponse events in a remembered per-agent channel`

---

### Task 3 — runtime `hitl/batch.rs`: one notification per response

**Interfaces.** Consumes Task 2. Produces `TaskRunner::with_decision_store(Arc<dyn DecisionStore>)`, the `tool/approval_needed` shape with `calls`, and `tool/hitl_respond` accepting `surface`. Task 4 (murmur) and Task 5 (Hub) consume the wire shape:
```jsonc
{ "method": "tool/approval_needed", "params": {
    "batch_id": "<uuid>", "task_id": "...", "timeout_ms": 300000,
    "step_id": "<first>", "hitl_id": "<first>", "tool_name": "<first>", "tool_input": {..},   // legacy = calls[0]
    "calls": [ { "hitl_id": "...", "step_id": "...", "tool_name": "...", "tool_input": {..}, "action_hash": "<64 hex>" }, ... ]
} }
```

- [ ] `mur-agent-runtime/src/hitl/batch.rs`:
  ```rust
  //! Gate B, batched: every `Ask` call of one LLM response is resolved before
  //! any of them runs — a settled decision from the store, or ONE
  //! `tool/approval_needed` carrying all of them. Each call keeps its own
  //! `hitl_id` and its own oneshot, so `tool/hitl_respond` is unchanged and no
  //! single answer can release more than one call (Global Constraint 1).

  use std::collections::HashMap;
  use std::sync::Arc;
  use std::time::Duration;

  use super::store::{DecisionStore, Settled, chat_action_hash};
  use super::{HitlApprovals, HitlDecision};

  /// One `Ask` call awaiting a decision.
  pub struct PendingCall {
      pub call_id: String,
      pub step_id: String,
      pub tool_name: String,
      pub tool_input: serde_json::Value,
      pub action_hash: String,
  }

  pub struct BatchGate<'a> {
      pub agent: &'a str,
      pub task_id: &'a str,
      pub timeout: Duration,
      pub approvals: &'a HitlApprovals,
      pub notifier: &'a tokio::sync::mpsc::Sender<serde_json::Value>,
      pub store: Option<&'a Arc<dyn DecisionStore>>,
  }

  impl BatchGate<'_> {
      /// Resolve every pending call. Returns `call_id → decision`; every input
      /// call has an entry (timeout and store-denials included).
      pub async fn resolve(&self, calls: Vec<PendingCall>) -> HashMap<String, HitlDecision> {
          let mut out = HashMap::with_capacity(calls.len());
          let mut ask = Vec::new();
          for c in calls {
              let settled = match self.store {
                  Some(s) => s.lookup(&c.action_hash).await,
                  None => None,
              };
              match settled {
                  Some(Settled::Allow) => {
                      out.insert(c.call_id, remembered(true));
                  }
                  Some(Settled::Deny) => {
                      out.insert(c.call_id, remembered(false));
                  }
                  None => ask.push(c),
              }
          }
          if ask.is_empty() {
              return out;
          }
          let batch_id = uuid::Uuid::now_v7().to_string();
          let mut waiting = Vec::with_capacity(ask.len());
          let mut wire = Vec::with_capacity(ask.len());
          {
              let mut pa = self.approvals.lock().await;
              for c in &ask {
                  let hitl_id = uuid::Uuid::now_v7().to_string();
                  let (tx, rx) = tokio::sync::oneshot::channel::<HitlDecision>();
                  pa.insert(hitl_id.clone(), tx);
                  wire.push(serde_json::json!({
                      "hitl_id": hitl_id,
                      "step_id": c.step_id,
                      "tool_name": c.tool_name,
                      "tool_input": c.tool_input,
                      "action_hash": c.action_hash,
                  }));
                  waiting.push((c, hitl_id, rx));
              }
          }
          let first = &wire[0];
          let notification = serde_json::json!({
              "jsonrpc": "2.0",
              "method": "tool/approval_needed",
              "params": {
                  "batch_id": batch_id,
                  "task_id": self.task_id,
                  "timeout_ms": self.timeout.as_millis() as u64,
                  // Legacy single-call fields = the first call, for clients that
                  // predate `calls` (D1). Their answer lands on calls[0]; the rest
                  // time out and deny, which is the fail-closed direction.
                  "step_id": first["step_id"],
                  "hitl_id": first["hitl_id"],
                  "tool_name": first["tool_name"],
                  "tool_input": first["tool_input"],
                  "calls": wire,
              }
          });
          let _ = self.notifier.send(notification).await;

          // One deadline for the whole batch; each oneshot is awaited in turn
          // against what is left of it.
          let deadline = tokio::time::Instant::now() + self.timeout;
          for (c, hitl_id, rx) in waiting {
              let decision = match tokio::time::timeout_at(deadline, rx).await {
                  Ok(Ok(d)) => d,
                  _ => {
                      self.approvals.lock().await.remove(&hitl_id);
                      HitlDecision {
                          allow: false,
                          reason: Some("timed out".into()),
                          surface: None,
                      }
                  }
              };
              if let Some(s) = self.store {
                  s.record(mur_common::hitl::HitlResponse {
                      hitl_id: hitl_id.clone(),
                      action_hash: c.action_hash.clone(),
                      allow: decision.allow,
                      reason: decision.reason.clone().unwrap_or_default(),
                      surface: decision.surface.clone().unwrap_or_else(|| "unknown".into()),
                  })
                  .await;
              }
              out.insert(c.call_id, decision);
          }
          out
      }
  }

  fn remembered(allow: bool) -> HitlDecision {
      HitlDecision {
          allow,
          reason: Some(if allow { "approved earlier".into() } else { "denied earlier".into() }),
          surface: None,
      }
  }

  /// Build the pending entry for one call. Separate so `task_runner` never
  /// spells the hash itself.
  pub fn pending(agent: &str, step_id: String, call: &crate::llm::ToolCallResult) -> PendingCall {
      PendingCall {
          call_id: call.call_id.clone(),
          step_id,
          action_hash: chat_action_hash(&call.tool_name, &call.input, agent),
          tool_name: call.tool_name.clone(),
          tool_input: call.input.clone(),
      }
  }
  ```
  Note: a timed-out call is recorded as a denial (`allow: false`, reason "timed out"). That means the next identical call inside the TTL is denied without asking. This is the fail-closed direction and matches gate A, where an unanswered request stays pending and a *settled* denial is remembered — but a timeout is not a human decision. **Do not record timeouts**: wrap the `record` in `if decision.reason.as_deref() != Some("timed out")`. Write it that way; the note stays as the reason.
- [ ] `task_runner.rs`: add the field and builder beside `pending_approvals`:
  ```rust
  /// P3: settled chat-gate decisions (gate B memory). `None` = ask every time.
  decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
  /// The agent's own name, for `chat_action_hash`. Empty until the supervisor
  /// sets it; an empty name still hashes, it just never matches another agent.
  agent_name: String,
  ```
  default `decision_store: None, agent_name: String::new()` in the constructor; builders:
  ```rust
  pub fn with_decision_store(mut self, s: Arc<dyn crate::hitl::store::DecisionStore>) -> Self {
      self.decision_store = Some(s);
      self
  }
  pub fn with_agent_name(mut self, name: impl Into<String>) -> Self {
      self.agent_name = name.into();
      self
  }
  ```
- [ ] `task_runner.rs` tool loop (the `for call in &resp.tool_calls` at ~1849) becomes:
  ```rust
  // P3: gate the whole response first — one notification, N decisions.
  let decisions = self.gate_response(task_id, &resp.tool_calls).await?;
  let mut results = Vec::new();
  for call in &resp.tool_calls {
      match self
          .handle_tool_call(task_id, call, decisions.get(&call.call_id).cloned())
          .await
      {
          Ok(entry) => results.push(entry),
          Err(e) => return Err(e),
      }
  }
  ```
  and add the method next to `handle_tool_call`:
  ```rust
  /// Resolve every `Ask` call of one response before any executes. Calls whose
  /// policy is not `Ask`, or whose tool is unknown, get no entry and take the
  /// existing Allow/Deny/unknown-tool arms. Fail-closed exactly as before: no
  /// sink → deny; a caller that declared `can_approve: false` → deny without
  /// asking (Global Constraint 3).
  async fn gate_response(
      &self,
      task_id: &str,
      calls: &[crate::llm::ToolCallResult],
  ) -> Result<std::collections::HashMap<String, crate::hitl::HitlDecision>, TaskError> {
      use mur_common::agent::ToolPolicy;
      let known: std::collections::HashSet<String> =
          self.tools_for_loop().iter().map(|t| t.name().to_string()).collect();
      let mut pending = Vec::new();
      let mut out = std::collections::HashMap::new();
      let entry = self.client_notifiers.lock().await.get(task_id).cloned();
      for call in calls {
          if !known.contains(&call.tool_name)
              || effective_tool_policy(&self.tools_policy, &call.tool_name) != ToolPolicy::Ask
          {
              continue;
          }
          if let Some(d) = decide_without_asking(entry.as_ref().map(|(_, ok)| *ok), &call.tool_name) {
              out.insert(call.call_id.clone(), d);
              continue;
          }
          pending.push(crate::hitl::batch::pending(
              &self.agent_name,
              uuid::Uuid::now_v7().to_string(),
              call,
          ));
      }
      if pending.is_empty() {
          return Ok(out);
      }
      let routed = entry.map(|(tx, _)| tx);
      let notifier = routed.as_ref().or(self.notifier.as_ref());
      let (Some(pa), Some(notifier)) = (&self.pending_approvals, notifier) else {
          for c in pending {
              out.insert(
                  c.call_id,
                  crate::hitl::HitlDecision {
                      allow: false,
                      reason: Some("no approval channel available".into()),
                      surface: None,
                  },
              );
          }
          return Ok(out);
      };
      let gate = crate::hitl::batch::BatchGate {
          agent: &self.agent_name,
          task_id,
          timeout: std::time::Duration::from_secs(self.hitl_timeout_secs as u64),
          approvals: pa,
          notifier,
          store: self.decision_store.as_ref(),
      };
      out.extend(gate.resolve(pending).await);
      Ok(out)
  }
  ```
  `handle_tool_call` gains a third parameter `decision: Option<crate::hitl::HitlDecision>`; its `ToolPolicy::Ask` arm is replaced entirely by:
  ```rust
  ToolPolicy::Ask => {
      // P3: the decision was made in `gate_response`, before any call of
      // this response ran. Absent = the gate never saw this call; deny,
      // never execute (Global Constraint 2).
      let decision = decision.unwrap_or(crate::hitl::HitlDecision {
          allow: false,
          reason: Some("no approval decision for this call".into()),
          surface: None,
      });
      if !decision.allow {
          return Err(task_error(
              "hitl_denied",
              deny_message(decision.reason.as_deref()),
              false,
          ));
      }
      // Approved: fall through to execute below.
  }
  ```
  The `step_id` used for `step/started` must be the one the notification carried, so the Hub/murmur can mark the card: `pending(..)` minted it. Simplest: `gate_response` returns `(decisions, step_ids: HashMap<call_id, step_id>)` and `handle_tool_call` takes `step_id: Option<String>` too, falling back to a fresh uuid. Do that; the two extra parameters are the whole change to the signature.
- [ ] `supervisor.rs` `HitlRespondHandler::handle`: after `let reason = ...` add `let surface = p["surface"].as_str().map(str::to_string);` and send `HitlDecision { allow, reason, surface }`. In `HitlTestRequestHandler` and every other `HitlDecision {` literal add `surface: None`.
- [ ] Wiring. In `supervisor.rs` where `build_provider_runner` returns `runner` (an `Arc<TaskRunner>`), the store cannot be added after the `Arc` — so `build_provider_runner` takes one more argument `decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>` and calls `.with_decision_store(s)` / `.with_agent_name(profile.inner.name.clone())` on the builder before wrapping. In `supervisor.rs` before the call:
  ```rust
  let decision_store: Arc<dyn crate::hitl::store::DecisionStore> =
      Arc::new(crate::hitl::store::ChannelDecisionStore::new(
          mur_home.clone(),
          profile.inner.name.clone(),
          identity.clone(),
          profile.inner.identity.key_version,
      ));
  ```
  and pass `Some(decision_store)`. Every other caller of `build_provider_runner` (grep) passes `None`.
- [ ] Tests in `task_runner.rs` tests module, after `ask_tool_executes_after_approval`:
  ```rust
  /// P3 §3.1: two `Ask` calls in one response → ONE `tool/approval_needed`
  /// carrying both, two pending oneshots, and both execute after two allows.
  #[tokio::test]
  async fn two_ask_calls_in_one_response_emit_one_notification() {
      use crate::llm::stub::SequenceLlm;
      let mut two = tool_call_response("c-1", "echo one");
      two.tool_calls.push(crate::llm::ToolCallResult {
          call_id: "c-2".into(),
          tool_name: "bash".into(),
          input: serde_json::json!({"command": "echo two"}),
      });
      let responses = vec![two, end_turn_response("DONE")];
      let calls = Arc::new(AtomicU64::new(0));
      let pa = empty_pending_approvals();
      let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
      let runner = Arc::new(
          TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
              .with_tools(vec![Arc::new(CountingBashTool { calls: calls.clone() })])
              .with_tools_policy(vec![])
              .with_pending_approvals(pa.clone())
              .with_notifier(ntx)
              .with_hitl_timeout_secs(5)
              .with_max_iterations(5),
      );
      let pa2 = pa.clone();
      let approver = tokio::spawn(async move {
          for _ in 0..500 {
              let senders: Vec<_> = {
                  let mut g = pa2.lock().await;
                  let keys: Vec<String> = g.keys().cloned().collect();
                  keys.into_iter().filter_map(|k| g.remove(&k)).collect()
              };
              for tx in senders {
                  let _ = tx.send(crate::hitl::HitlDecision { allow: true, reason: None, surface: None });
              }
              tokio::time::sleep(std::time::Duration::from_millis(10)).await;
          }
      });
      let outcome = runner.run_sync(loop_spec("batch")).await;
      approver.abort();
      assert!(matches!(outcome, TaskOutcome::Completed(_)), "{outcome:?}");
      assert_eq!(calls.load(Ordering::Relaxed), 2);
      let mut approvals = 0;
      let mut batch_len = 0;
      while let Ok(n) = nrx.try_recv() {
          if n["method"] == "tool/approval_needed" {
              approvals += 1;
              batch_len = n["params"]["calls"].as_array().map(|a| a.len()).unwrap_or(0);
              assert_eq!(n["params"]["hitl_id"], n["params"]["calls"][0]["hitl_id"], "legacy fields = calls[0]");
              assert_eq!(n["params"]["calls"][0]["action_hash"].as_str().map(str::len), Some(64));
          }
      }
      assert_eq!(approvals, 1, "one notification for the whole response");
      assert_eq!(batch_len, 2);
  }

  /// P3 §3.2: a remembered allow executes without asking; a remembered deny
  /// denies without asking; nothing is asked in either case.
  #[tokio::test]
  async fn remembered_decisions_are_not_asked_again() {
      use crate::hitl::store::{DecisionStore, Settled};
      struct Fixed(Settled);
      #[async_trait::async_trait]
      impl DecisionStore for Fixed {
          async fn lookup(&self, _h: &str) -> Option<Settled> { Some(self.0) }
          async fn record(&self, _r: mur_common::hitl::HitlResponse) {}
      }
      for (settled, expect_calls, expect_completed) in [
          (Settled::Allow, 1u64, true),
          (Settled::Deny, 0u64, false),
      ] {
          use crate::llm::stub::SequenceLlm;
          let responses = vec![tool_call_response("c-1", "echo hi"), end_turn_response("OK")];
          let calls = Arc::new(AtomicU64::new(0));
          let (ntx, mut nrx) = tokio::sync::mpsc::channel(16);
          let runner = Arc::new(
              TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                  .with_tools(vec![Arc::new(CountingBashTool { calls: calls.clone() })])
                  .with_tools_policy(vec![])
                  .with_pending_approvals(empty_pending_approvals())
                  .with_notifier(ntx)
                  .with_decision_store(Arc::new(Fixed(settled)))
                  .with_hitl_timeout_secs(1)
                  .with_max_iterations(3),
          );
          let outcome = runner.run_sync(loop_spec("remembered")).await;
          assert_eq!(matches!(outcome, TaskOutcome::Completed(_)), expect_completed, "{settled:?}: {outcome:?}");
          assert_eq!(calls.load(Ordering::Relaxed), expect_calls, "{settled:?}");
          while let Ok(n) = nrx.try_recv() {
              assert_ne!(n["method"], "tool/approval_needed", "{settled:?} must not ask");
          }
      }
  }
  ```
  `CountingBashTool`: if no such helper exists (`grep -n "struct CountingBashTool" task_runner.rs`), add one beside `CountingFleetRunTool`, same shape, `name()` returns `"bash"`, `execute` bumps the counter and returns `ToolOutput { text: "ok".into(), status: ToolStatus::Ok, images: vec![] }` (copy the exact `ToolOutput` field set from `CountingFleetRunTool`).
- [ ] The existing gate tests must still pass unchanged: `ask_tool_denies_when_no_approval_sink`, `ask_tool_executes_after_approval`, `fleet_run_explicit_deny_still_wins`, `hitl_respond_*` in `supervisor.rs`, and every test that constructed `HitlDecision` (now with `surface: None`).
- [ ] `set -o pipefail; cargo nextest run -p mur-agent-runtime --lib 2>&1 | tail -n 8` → all pass (the whole lib, not just `hitl`: the loop changed). `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings` → 0. `cargo fmt -p mur-agent-runtime`. `wc -l mur-agent-runtime/src/task_runner.rs` → fewer lines than before this task (the inline ask block left; two tests came in — if it grew, move the two new tests to `hitl/batch.rs` tests instead).
- [ ] Commit: `feat(runtime): chat gate asks once per response and remembers settled decisions`

---

### Task 4 — murmur: expand `calls`

**Interfaces.** Consumes the Task 3 wire shape. Produces `HitlRequest::from_params(v: Value) -> Vec<HitlRequest>` in `mur-core/src/cmd/agent/cli/stream.rs`. Nothing downstream changes: murmur's queue already holds many requests and answers each by its own `hitl_id`.

- [ ] `stream.rs`: below `from_value` add:
  ```rust
  /// One request per gated call. A P3 runtime sends `calls: [...]`; each entry
  /// has its own `hitl_id`. Older runtimes send only the top-level fields,
  /// which are exactly one call — so the fallback is `vec![from_value(v)]`.
  pub fn from_params(v: Value) -> Vec<Self> {
      match v.get("calls").and_then(Value::as_array) {
          Some(calls) if !calls.is_empty() => calls.iter().cloned().map(Self::from_value).collect(),
          _ => vec![Self::from_value(v)],
      }
  }
  ```
  `from_value` reads `step_id`, `hitl_id`, `tool_name`, `tool_input`, `prompt` — every per-call entry carries the first four; `prompt` falls back to `Run \`<tool>\`?` as today.
- [ ] The `on_hitl` closure (~line 262) becomes:
  ```rust
  |hitl| {
      for req in HitlRequest::from_params(hitl) {
          let _ = tx.blocking_send(StreamMsg::Hitl {
              task_id: tid.clone(),
              req,
          });
      }
  },
  ```
- [ ] `stream.rs` tests module, beside the existing `from_value` tests:
  ```rust
  #[test]
  fn from_params_expands_calls_and_falls_back_to_single() {
      let batch = json!({
          "hitl_id": "h1", "tool_name": "bash", "tool_input": {"command": "a"},
          "calls": [
              {"hitl_id": "h1", "step_id": "s1", "tool_name": "bash", "tool_input": {"command": "a"}},
              {"hitl_id": "h2", "step_id": "s2", "tool_name": "write_file", "tool_input": {"path": "x"}}
          ]
      });
      let reqs = HitlRequest::from_params(batch);
      assert_eq!(reqs.iter().map(|r| r.hitl_id.as_str()).collect::<Vec<_>>(), ["h1", "h2"]);
      assert_eq!(reqs[1].step_id.as_deref(), Some("s2"));
      let single = HitlRequest::from_params(json!({"hitl_id": "h9", "tool_name": "bash"}));
      assert_eq!(single.len(), 1);
      assert_eq!(single[0].hitl_id, "h9");
  }
  ```
- [ ] Confirm the queue: `grep -n "pending_hitl\|hitl_queue\|VecDeque<HitlRequest>\|Vec<HitlRequest>" mur-core/src/cmd/agent/cli/*.rs` shows the collection `StreamMsg::Hitl` pushes into. If it is a single `Option<HitlRequest>` (not a collection), STOP and report — batching would drop N-1 requests in murmur and the plan needs a queue step first.
- [ ] `set -o pipefail; RUST_MIN_STACK=33554432 cargo nextest run -p mur-core --lib cmd::agent::cli::stream 2>&1 | tail -n 6` → passes. `cargo clippy -p mur-core --all-targets -- -D warnings` → 0. `cargo fmt -p mur-core`.
- [ ] Commit: `feat(murmur): one approval card per call when the runtime batches a response`

---

### Task 5 — Hub backend: relay the batch, say `hub`

**Interfaces.** Consumes Task 3's wire shape. Produces the `hitl-approval-needed` Tauri event with `calls` and `batch_id`; `agent_hitl_respond` sends `surface: "hub"`. Task 6 consumes the event.

- [ ] `mur-hub-gui/src-tauri/src/chat.rs` relay closure: add two fields:
  ```rust
  "batch_id": hitl_params.get("batch_id"),
  "calls": hitl_params.get("calls"),
  ```
  (legacy fields stay so `useInbox`/`ConversationContext`, which only read `agent`, keep working).
- [ ] `mur-hub-gui/src-tauri/src/hitl.rs` `agent_hitl_respond`: `let mut payload = json!({ "hitl_id": hitl_id, "allow": allow, "surface": "hub" });`
- [ ] `cd mur-hub-gui/src-tauri && set -o pipefail; cargo clippy --all-targets -- -D warnings 2>&1 | tail -n 5` → 0 (needs `ui/dist`; run `npm run build` in `ui/` first if missing).
- [ ] Commit: `feat(hub): relay batched approvals; the Hub says which surface answered`

---

### Task 6 — Hub UI: the card that asks once and can decide forever

**Interfaces.** Consumes Task 5's event and P1's `PermissionsView` (`invoke<AgentDetail>("get_agent_detail", { name })` → `.permissions`) and P2's `agent_perm_set_tool(name, pattern, policy)` / `agent_perm_grant_path(name, verb, path)` (confirm exact argument names with `grep -n "pub fn agent_perm_set_tool\|pub fn agent_perm_grant_path" -A6 mur-hub-gui/src-tauri/src/perm_admin.rs` and use the snake→camel names Tauri expects, as `PermissionEditors.tsx` already does).

- [ ] `types.ts`: extend `HitlRequest` with `action_hash?: string; batch_id?: string;` and add:
  ```ts
  /** `hitl-approval-needed` payload: legacy single-call fields plus, from a P3 runtime, `calls`. */
  export interface HitlBatchPayload extends HitlRequest {
    calls?: Array<Pick<HitlRequest, "hitl_id" | "tool_name" | "tool_input" | "action_hash"> & { step_id?: string }>;
  }
  ```
- [ ] `mur-hub-gui/ui/src/components/hitlModel.ts`:
  ```ts
  import type { HitlBatchPayload, HitlRequest, PermissionsView } from "../types";

  /** One card per call. Legacy payloads (no `calls`) are one call. */
  export function expandBatch(p: HitlBatchPayload): HitlRequest[] {
    const base = { agent: p.agent, prompt: p.prompt, timeout_ms: p.timeout_ms, batch_id: p.batch_id };
    if (p.calls && p.calls.length > 0) {
      return p.calls.map((c) => ({ ...base, hitl_id: c.hitl_id, tool_name: c.tool_name, tool_input: c.tool_input, action_hash: c.action_hash }));
    }
    return [{ ...base, hitl_id: p.hitl_id, tool_name: p.tool_name, tool_input: p.tool_input, action_hash: p.action_hash }];
  }

  /** `mcp__srv__tool` → `tool`; anything else unchanged. */
  export function leafToolName(name: string): string {
    const m = name.match(/^mcp__[^_]+(?:_[^_]+)*__(.+)$/);
    return m ? m[1] : name;
  }

  /** Spend / dispatch tools never get an "always" (spec §3.4, Testing): the
   *  whole point of `Ask` on them is that every spend is a human decision. */
  export const NO_ALWAYS_TOOLS = ["fleet_run", "parallel_jobs", "delegate_to"] as const;

  /** The exact-name rule "always" would write, or null when no rule is offered.
   *  Never a glob: the pattern is the full tool name as the runtime spells it. */
  export function alwaysRuleFor(toolName: string): { pattern: string; policy: "allow" } | null {
    if ((NO_ALWAYS_TOOLS as readonly string[]).includes(leafToolName(toolName))) return null;
    if (toolName.includes("*")) return null;
    return { pattern: toolName, policy: "allow" };
  }

  const PATH_TOOLS: Record<string, "read" | "write"> = {
    read_file: "read",
    write_file: "write",
    edit_file: "write",
  };

  /** When the call names a path outside the agent's grants, the P2 grant that
   *  would let it SUCCEED. Separate from "always" (Constraint 3): a rule answers
   *  "may it run", a grant answers "can it reach". */
  export function grantHintFor(
    toolName: string,
    input: Record<string, unknown>,
    perms: PermissionsView | null,
  ): { verb: "read" | "write"; path: string } | null {
    const verb = PATH_TOOLS[leafToolName(toolName)];
    const path = typeof input.path === "string" ? input.path : null;
    if (!verb || !path || !perms) return null;
    const granted = (verb === "read" ? [...perms.filesystem.read, ...perms.filesystem.write] : perms.filesystem.write)
      .map((g) => g.expanded.replace(/\/+$/, ""));
    const covered = granted.some((g) => path === g || path.startsWith(g + "/"));
    return covered ? null : { verb, path: parentDir(path) };
  }

  /** Grants are folders; a file path is offered as its directory. */
  function parentDir(p: string): string {
    const i = p.lastIndexOf("/");
    return i > 0 ? p.slice(0, i) : p;
  }
  ```
  `PermissionsView.filesystem.read[i].expanded` is the absolute path P1 already computes (`PathGrantView`). Import `PermissionsView` from `types.ts` (it is exported there, line 138).
- [ ] `hitlModel.test.ts`:
  ```ts
  import { describe, it, expect } from "vitest";
  import { expandBatch, alwaysRuleFor, grantHintFor, leafToolName } from "./hitlModel";
  import type { PermissionsView } from "../types";

  const base = { agent: "qa", prompt: "", timeout_ms: 1000, hitl_id: "h0", tool_name: "bash", tool_input: {} };

  describe("expandBatch", () => {
    it("legacy payload is one call", () => {
      expect(expandBatch(base).map((r) => r.hitl_id)).toEqual(["h0"]);
    });
    it("calls become one request each, keeping their own hitl_id and hash", () => {
      const reqs = expandBatch({
        ...base,
        batch_id: "b1",
        calls: [
          { hitl_id: "h1", tool_name: "bash", tool_input: { command: "a" }, action_hash: "x".repeat(64) },
          { hitl_id: "h2", tool_name: "write_file", tool_input: { path: "/tmp/f" }, action_hash: "y".repeat(64) },
        ],
      });
      expect(reqs.map((r) => [r.hitl_id, r.tool_name, r.batch_id])).toEqual([["h1", "bash", "b1"], ["h2", "write_file", "b1"]]);
      expect(reqs[1].action_hash).toBe("y".repeat(64));
    });
  });

  describe("alwaysRuleFor", () => {
    it("is the exact tool name, never a glob", () => {
      expect(alwaysRuleFor("mcp__research-gateway__search")).toEqual({ pattern: "mcp__research-gateway__search", policy: "allow" });
      expect(alwaysRuleFor("bash")?.pattern).toBe("bash");
      expect(alwaysRuleFor("mcp__x__*")).toBeNull();
    });
    it("offers nothing for spend/dispatch tools, however they are namespaced", () => {
      expect(alwaysRuleFor("fleet_run")).toBeNull();
      expect(alwaysRuleFor("mcp__mur__parallel_jobs")).toBeNull();
      expect(alwaysRuleFor("delegate_to")).toBeNull();
    });
  });

  describe("grantHintFor", () => {
    const perms = {
      filesystem: {
        read: [{ raw: "~/docs", expanded: "/Users/me/docs", status: "installed" }],
        write: [{ raw: "~/out", expanded: "/Users/me/out/", status: "installed" }],
        deny: [],
      },
    } as unknown as PermissionsView;
    it("is null when the path is inside a grant (write grants also cover reads)", () => {
      expect(grantHintFor("read_file", { path: "/Users/me/docs/a.md" }, perms)).toBeNull();
      expect(grantHintFor("read_file", { path: "/Users/me/out/b" }, perms)).toBeNull();
      expect(grantHintFor("write_file", { path: "/Users/me/out/b" }, perms)).toBeNull();
    });
    it("offers the parent folder with the verb the tool needs", () => {
      expect(grantHintFor("write_file", { path: "/Users/me/docs/a.md" }, perms)).toEqual({ verb: "write", path: "/Users/me/docs" });
      expect(grantHintFor("read_file", { path: "/tmp/x/y.txt" }, perms)).toEqual({ verb: "read", path: "/tmp/x" });
    });
    it("is null for tools without a path, and without permissions to compare against", () => {
      expect(grantHintFor("bash", { command: "ls" }, perms)).toBeNull();
      expect(grantHintFor("read_file", { path: "/tmp/x" }, null)).toBeNull();
    });
  });

  describe("leafToolName", () => {
    it("strips the mcp prefix only", () => {
      expect(leafToolName("mcp__research-gateway__search")).toBe("search");
      expect(leafToolName("bash")).toBe("bash");
    });
  });
  ```
- [ ] `HitlCard.tsx`: props become `{ request: HitlRequest; perms: PermissionsView | null; isRunning: boolean }`. Replace the non-reason actions block with:
  ```tsx
  <div className="hitl-card__actions">
    <button className="hitl-card__btn hitl-card__btn--allow" onClick={() => respond(true)} disabled={busy}>
      {t("hitl.allowOnce")}
    </button>
    {always && (
      <button
        className="hitl-card__btn hitl-card__btn--always"
        title={t("hitl.alwaysHint", { tool: request.tool_name })}
        onClick={() => respondAlways()}
        disabled={busy}
      >
        {t("hitl.always")}
      </button>
    )}
    <button className="hitl-card__btn hitl-card__btn--deny" onClick={() => respond(false)} disabled={busy}>
      {t("hitl.deny")}
    </button>
    <button className="hitl-card__btn hitl-card__btn--deny-reason" onClick={() => setShowReasonInput(true)} disabled={busy}>
      {t("hitl.denyWithReason")}
    </button>
  </div>
  {grant && (
    <div className="hitl-card__grant">
      <span>{t("hitl.grantHint", { path: grant.path })}</span>
      <button className="hitl-card__btn" onClick={() => doGrant()} disabled={busy || granted}>
        {granted ? "✓" : t("hitl.grant", { verb: grant.verb, path: grant.path })}
      </button>
    </div>
  )}
  {ruleWritten && <div className="hitl-card__hint">{t(isRunning ? "perm.restartHint" : "perm.saved")}</div>}
  ```
  with, above the early returns:
  ```tsx
  const always = alwaysRuleFor(request.tool_name);
  const grant = grantHintFor(request.tool_name, request.tool_input, perms);
  const [ruleWritten, setRuleWritten] = useState(false);
  const [granted, setGranted] = useState(false);

  // "Always" = the one-time allow (releases the open gate now) THEN the exact
  // rule (takes effect on restart). Never the other way round: a rule write
  // cannot release a gate that is open now (spec Constraint 4).
  async function respondAlways() {
    if (!always || responded || busy) return;
    await respond(true);
    try {
      await invoke("agent_perm_set_tool", { name: request.agent, pattern: always.pattern, policy: always.policy });
      setRuleWritten(true);
    } catch (e) {
      setError(String(e));
    }
  }

  async function doGrant() {
    if (!grant) return;
    setBusy(true);
    try {
      await invoke("agent_perm_grant_path", { name: request.agent, verb: grant.verb, path: grant.path });
      setGranted(true);
      setRuleWritten(true); // same restart hint
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  const [error, setError] = useState<string | null>(null);
  ```
  and `{error && <div className="hitl-card__error">{error}</div>}` under the actions. `respond` keeps its shape; the resolved states (`allowed` / `denied` / `timeout`) still render as today but the `allowed` state also shows `ruleWritten` hint when set. `t(key, vars)` — confirm the i18n helper's interpolation signature with `grep -n "export function useT\|function t(" mur-hub-gui/ui/src/i18n/index.tsx`; if it takes no vars, build the strings with template literals around `t("hitl.always")` etc. and keep the keys plain.
- [ ] `ChatTab.tsx`: the listener becomes
  ```tsx
  const unHitl = listen<HitlBatchPayload>("hitl-approval-needed", (e) => {
    if (e.payload.agent !== agentName) return;
    setHitlRequests((prev) => [...prev, ...expandBatch(e.payload)]);
  });
  ```
  add `const [perms, setPerms] = useState<PermissionsView | null>(null);` loaded once per `agentName`:
  ```tsx
  useEffect(() => {
    let live = true;
    invoke<AgentDetail>("get_agent_detail", { name: agentName })
      .then((d) => { if (live) setPerms(d.permissions ?? null); })
      .catch(() => { if (live) setPerms(null); });
    return () => { live = false; };
  }, [agentName]);
  ```
  (`AgentDetail.permissions` is the P1 field; confirm its name in `types.ts` with `grep -n "permissions" mur-hub-gui/ui/src/types.ts`.) `isRunning`: ChatTab already knows whether the agent is running if it renders a status; if not, pass `isRunning={true}` — the restart hint is the safe default (saying "restart" to a stopped agent costs nothing; omitting it for a running one misleads). Render grouped: consecutive requests sharing a `batch_id` sit under one `<div className="hitl-batch">` with a header `t("hitl.batchTitle", { n })` when `n > 1`; each still renders its own `<HitlCard>`.
- [ ] i18n, both files, same commit:
  | key | en | zh-TW |
  |---|---|---|
  | `hitl.allowOnce` | Allow this time | 這次允許 |
  | `hitl.always` | Always allow | 一律允許 |
  | `hitl.alwaysHint` | Writes an allow rule for exactly `{tool}` — takes effect when the agent restarts. Entitlements are not changed. | 為 `{tool}` 寫入一條 allow 規則，agent 重啟後生效；不會改動 entitlement。 |
  | `hitl.grantHint` | This path is outside the agent's grants; approving lets it run, not reach the file. | 這個路徑不在 agent 的授權範圍內；核准只是讓它執行，不代表它碰得到檔案。 |
  | `hitl.grant` | Grant {verb} on {path} | 授權 {path} 的 {verb} |
  | `hitl.batchTitle` | {n} approvals for this response | 這則回覆需要 {n} 個核准 |
  `perm.restartHint` / `perm.saved` exist from P2. Brand: no brand string here.
- [ ] `chat.css`: `.hitl-card__btn--always { background: var(--accent-muted); color: var(--accent); }` (use the tokens `.hitl-card__btn--allow` neighbours use — if those are raw hex, keep the same raw hex style for consistency and note it; do not introduce a third style). `.hitl-card__grant { display:flex; gap: var(--space-2); align-items:center; font-size: var(--text-sm); color: var(--fg-muted); }`, `.hitl-batch { border-left: 2px solid var(--border); padding-left: var(--space-2); }`, `.hitl-card__error { color: var(--danger); font-size: var(--text-sm); }`.
- [ ] `cd mur-hub-gui/ui && npm test -- --run 2>&1 | tail -n 8` → all pass incl. `hitlModel`. `npm run build 2>&1 | tail -n 3` → built. `npx tsc --noEmit` → 0 errors.
- [ ] Commit: `feat(hub): approval card asks once per response, offers always-allow as an exact rule and the grant that makes it reach`

---

## Manual verification (real Hub, before the PR is marked ready)

1. `dr_worker_1` (policy default Ask for `write_file`): ask it to write two files in one reply. Expect ONE batch header "2 approvals for this response" with two cards; approve both; both files exist.
2. Ask for the same two writes again inside a minute. Expect no card; the reply completes; `mur channel show $(cat ~/.mur/agents/dr_worker_1/hitl-channel)` lists two `HitlResponse` events with `surface: hub`.
3. On a fresh `read_file` of a path outside grants: the card shows the grant control; press it; P2 Permissions section shows the new folder; restart hint shown.
4. Press *Always allow* on a `bash` card: the reply completes; Permissions → tool rules shows `bash: allow`; `mur agent perm tool-list dr_worker_1` agrees.
5. Ask for `fleet_run`: the card has no *Always allow*.
6. murmur: `murmur dr_worker_1`, same two-file request → two inline approval rows, each answerable.

## Self-review

- Spec coverage: §3.1 → Task 3 (+4, 5, 6 presentation). §3.2 → Tasks 1, 2, 3. §3.3 → Task 6 (+5 `surface`). §3.4 → Global Constraints 1–3, D2, `NO_ALWAYS_TOOLS`. Testing bullet → Task 3 tests (N calls / one notification; settled allow, deny), Task 2 (expired, unsigned), Task 6 tests (exact-name only, no projection for spend tools).
- Placeholders: none; every `confirm with grep` step names the grep and what to do with each outcome.
- Cross-task names: `chat_action_hash` (T2) used by `batch::pending` (T3); `DecisionStore`/`Settled` (T2) used by `BatchGate` and the `Fixed` test store (T3); `HitlDecision.surface` (T2) set in `HitlRespondHandler` (T3) and sent by `agent_hitl_respond` (T5); wire field names `calls[].hitl_id/step_id/tool_name/tool_input/action_hash`, `batch_id` identical in T3, T4 (`from_params`), T5 (relay), T6 (`expandBatch`).
