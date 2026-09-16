# Durable Monitor Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the durable-monitor engine speak — deliver the notable events it already records to a desktop notification and the daemon log, once each, with a message that names the one thing the user should do next.

**Architecture:** `mur-monitor` owns the queue: which event kinds are notifiable, what the message says (pure), and the delivery-state rows. `mur-core` owns the channels, because delivery spawns processes and `mur-monitor` sits below it deliberately. The daemon's existing 15 s tick drains the queue after each pass — no second loop. A delivery that fails is retried with its own backoff and its state is visible in `mur monitor show`.

**Tech Stack:** Rust 2024, `rusqlite` 0.32 (the existing `monitors.db`), `mur_common::config`, the `osascript`/`notify-send` shape already used by `mur-core/src/cmd/agent/cli/notify.rs`, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-09-11-durable-monitor-design.md` — this plan implements **step 9's notification half** (§通知策略). The metrics half of §可觀測性 is deliberately out of scope: it has no consumer until a dashboard exists, and shipping counters nobody reads is how you get counters nobody maintains. Steps 5–8 are separate plans.

## Global Constraints

- **Normal polling notifies nobody.** Only the kinds listed in §通知策略 are notifiable: first entry to `stalled` (and one `stalled_recovered`), first crossing of the soft deadline, an approval becoming required (plan-2b), automatic remediation failing or hitting its ceiling (plan-2b), a terminal settlement, and the monitor itself being persistently `unknown` / credential-broken / adapter-broken. `created`, `retried`, `lease_recovered`, `hard_deadline` and every observation are **not** notifiable.
- **A notification message must carry**, per §通知策略, all of: the monitor name, the source reference, the known outcome, the actions taken, the next check time, and **the single step the user should take**. Until plan-2b exists there are no actions, and that field renders as `—` rather than being omitted — the shape is part of the contract.
- **Secrets never reach a notification.** Evidence is already redacted at the adapter chokepoint; the notifier must not re-read a `SecretRef` or interpolate `credential_ref`'s value. Only the reference string may appear.
- **Delivery failure never rolls anything back** and never fails a tick (§錯誤處理). It retries on its own backoff and surfaces its state in the CLI.
- **`monitor_notifications` already exists**, created empty by plan-1: `(event_key TEXT PRIMARY KEY, monitor_id TEXT NOT NULL, channel TEXT NOT NULL, delivery_state TEXT NOT NULL, updated_at TEXT NOT NULL)`. It has never been written to, so its shape is free to use as-is; do not alter it (plan-1's `migrate()` is `CREATE TABLE IF NOT EXISTS`, so an `ALTER` would need a `user_version` step this plan does not want to introduce).
- **Every source file ≤ 800 lines** (CLAUDE.md rule 4). This branch's two predecessors both hit the cap and split rather than compressing comments; do the same.
- **Test runner is `cargo nextest`**, never bare `cargo test` — plain `cargo test` has a known `MUR_HOME` cross-test race that nextest's per-test process avoids. For `mur-core`: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core <filter>`.
- **Brand is uppercase `MUR`** in anything a user reads (CLAUDE.md rule 7) — including notification titles.

## The spec defect this plan corrects — read before Task 1

§通知策略 says *"每種 event 以 monitor＋cycle＋event type 去重"*. **Do not implement that.** It was written before plan-1 established that `cycle_id` never rotates for a monitor, and keying on `(monitor, cycle, kind)` would silence every episode after the first — reintroducing at the notification layer the exact defect plan-1's final review caught and fixed in the events layer (`monitor_unhealthy` and `stalled` were moved to `dedup: false` for precisely this reason).

**Key on the event row's own `id` instead.** `monitor_events` is `(id INTEGER PRIMARY KEY AUTOINCREMENT, …)`, and the semantic dedup the spec wants already happened when the event was written: every notifiable kind is guarded by a transition condition (`stalled_newly`, `recovered`, `soft_newly`, `streak == UNHEALTHY_AFTER_UNKNOWN`, a terminal state that is not claimable again). One event row **is** one real transition. So one row → at most one notification per channel, and a second episode gets a second row and therefore a second notification.

## File structure

```
mur-monitor/src/notify.rs            notifiable kinds + the pure message   (Task 1)
mur-monitor/src/store/notify.rs      queue ops on monitor_notifications     (Task 2)
mur-core/src/monitor/notify/mod.rs   Channel trait + registry               (Task 3)
mur-core/src/monitor/notify/log.rs   the always-on log channel              (Task 3)
mur-core/src/monitor/notify/desktop.rs  osascript / notify-send             (Task 4)
mur-common/src/config.rs             `notifications:` block                 (Task 5)
mur-core/src/monitor/service.rs      drain_notifications()                  (Task 6)
mur-daemon/src/monitor_tick.rs       call it after each tick                (Task 6)
mur-core/src/cmd/monitor.rs          delivery state in `show`               (Task 7)
CLAUDE.md, README.md                 the user-facing description            (Task 7)
```

## Decisions already made — do not relitigate

1. **Two channels only: `log` and `desktop`.** Slack, the companion and mobile push are not in §通知策略's MVP and each needs its own credential story. The `Channel` trait is the seam for adding them later; adding one now would be speculative.
2. **`log` is always on and cannot be disabled.** A monitor that went unhealthy must leave a trace somewhere even when a user has turned notifications off, and the daemon log is where an operator already looks.
3. **Desktop is opt-in, default OFF.** A background daemon that starts popping OS notifications the moment a user upgrades is a hostile default. `notifications.desktop: true` in `config.yaml` turns it on.
4. **The drain runs on the existing tick, not a new thread.** Delivery is fast (a spawn) and bounded (`DRAIN_MAX_PER_TICK`); a second loop would be a second thing to supervise for no gain.
5. **A monitor with no store gets no drain.** Same rule plan-1 established for the tick itself: `open_existing` and no-op. A user who has never run `mur monitor` acquires nothing.
6. **Retry is bounded and then parked, not infinite.** `DELIVERY_MAX_ATTEMPTS = 5` with the existing `backoff::unknown_delay` curve, then `delivery_state = "failed"`, visible in `show`. An undeliverable notification that retries forever is a busy loop with a nice name.

---

### Task 1: Notifiable kinds and the pure message

**Files:**
- Create: `mur-monitor/src/notify.rs`
- Modify: `mur-monitor/src/lib.rs` (add `pub mod notify;` — alphabetically, after `deadline`)
- Test: `mur-monitor/src/notify.rs` (`mod tests`)

**Interfaces:**
- Consumes: `store::{MonitorRow, EventRow}`, `state::Outcome`
- Produces:
  ```rust
  pub const NOTIFIABLE: &[&str]
  pub fn is_notifiable(kind: &str) -> bool
  pub struct Notification { pub title: String, pub body: String, pub next_step: String }
  pub fn render(row: &MonitorRow, event: &EventRow) -> Notification
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::MonitorSpec;
    use crate::state::{MonitorState, Outcome};
    use chrono::{TimeZone, Utc};

    fn spec() -> MonitorSpec {
        MonitorSpec::from_yaml(
            "schema_version: 1\nname: wait-for-ci\nsource: { type: github_actions, reference: mur-run/mur/123 }\nidempotency_key: k\ncreated_by: { actor: user:test }\n",
        )
        .unwrap()
    }

    fn row(state: MonitorState, outcome: Outcome) -> MonitorRow {
        let t = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap();
        MonitorRow {
            id: "01a0-aaaa".into(),
            name: "wait-for-ci".into(),
            spec: spec(),
            state,
            outcome,
            source_type: crate::spec::SourceType::GithubActions,
            reference: "mur-run/mur/123".into(),
            idempotency_key: "k".into(),
            created_at: t,
            work_started_at: t,
            next_check_at: t + chrono::Duration::minutes(5),
            last_checked_at: Some(t),
            last_progress_at: t,
            progress_token: None,
            pending_attempts: 3,
            unknown_streak: 0,
            remediation_attempts: 0,
            cycle_id: "cyc".into(),
            stalled_since: None,
            soft_notified: false,
            hard_reached: false,
            fence: 1,
            version: 2,
        }
    }

    fn event(kind: &str) -> EventRow {
        EventRow {
            cycle_id: "cyc".into(),
            kind: kind.into(),
            payload: serde_json::json!({}),
            created_at: Utc.with_ymd_and_hms(2026, 9, 15, 12, 5, 0).unwrap(),
        }
    }

    #[test]
    fn only_the_spec_listed_kinds_notify() {
        for k in ["stalled", "stalled_recovered", "soft_deadline", "terminal", "monitor_unhealthy", "exhausted"] {
            assert!(is_notifiable(k), "{k} must notify");
        }
        // Routine bookkeeping must never reach a user.
        for k in ["created", "retried", "lease_recovered", "observed", "cancelled", "hard_deadline"] {
            assert!(!is_notifiable(k), "{k} must NOT notify");
        }
    }

    #[test]
    fn every_message_carries_the_six_required_fields() {
        // spec §通知策略: name, source reference, known outcome, actions
        // taken, next check time, and the single step for the user.
        let n = render(&row(MonitorState::Sleeping, Outcome::Pending), &event("stalled"));
        for needle in ["wait-for-ci", "mur-run/mur/123", "pending", "—", "2026-09-15T12:05", "mur monitor show"] {
            assert!(
                n.body.contains(needle) || n.next_step.contains(needle),
                "missing {needle:?} in body={:?} next_step={:?}",
                n.body,
                n.next_step
            );
        }
        assert!(n.title.contains("MUR"), "brand is uppercase: {:?}", n.title);
    }

    #[test]
    fn the_next_step_differs_by_kind_and_is_never_empty() {
        let mut steps = std::collections::HashSet::new();
        for k in NOTIFIABLE {
            let n = render(&row(MonitorState::Sleeping, Outcome::Pending), &event(k));
            assert!(!n.next_step.trim().is_empty(), "{k} has no next step");
            steps.insert(n.next_step.clone());
        }
        assert!(steps.len() > 1, "every kind got the same next step — the field is decoration");
    }

    #[test]
    fn an_exhausted_monitor_is_told_retry_will_not_help_when_the_deadline_passed() {
        let mut r = row(MonitorState::Exhausted, Outcome::Unknown);
        r.hard_reached = true;
        let n = render(&r, &event("exhausted"));
        assert!(
            n.next_step.contains("hard deadline") && !n.next_step.contains("mur monitor retry"),
            "must not suggest a retry that refuses: {:?}",
            n.next_step
        );
    }

    #[test]
    fn no_credential_reference_value_reaches_the_message() {
        let mut r = row(MonitorState::Sleeping, Outcome::Unknown);
        r.spec.source.credential_ref = Some("keychain:mur/github-token".into());
        let n = render(&r, &event("monitor_unhealthy"));
        let all = format!("{} {} {}", n.title, n.body, n.next_step);
        assert!(!all.contains("keychain:"), "a credential reference must not be rendered: {all}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor notify::`
Expected: compile error — `notify` module missing.

- [ ] **Step 3: Implement**

```rust
//! Which recorded events are worth a human's attention, and what each one
//! says (spec §通知策略). Pure: no I/O, no channels, no clock — `render`
//! takes the row and the event and returns text. The delivery side is
//! `store::notify` (the queue) and `mur-core`'s channels.

use crate::state::MonitorState;
use crate::store::{EventRow, MonitorRow};

/// Exactly the kinds §通知策略 lists. Everything else a monitor records —
/// `created`, `retried`, `lease_recovered`, every observation — is
/// bookkeeping and must never reach a user. `hard_deadline` is deliberately
/// absent: crossing it is not itself news (the monitor either keeps polling
/// read-only or emits `exhausted`, and `exhausted` IS notifiable).
pub const NOTIFIABLE: &[&str] = &[
    "stalled",
    "stalled_recovered",
    "soft_deadline",
    "terminal",
    "monitor_unhealthy",
    "exhausted",
];

pub fn is_notifiable(kind: &str) -> bool {
    NOTIFIABLE.contains(&kind)
}

/// One rendered notification. `next_step` is separate from `body` so a
/// channel with a short field (a desktop banner) can lead with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub next_step: String,
}

/// The single action the user should take. §通知策略 requires one — not a
/// list of options, which is how a notification becomes something people
/// dismiss without reading.
fn next_step(row: &MonitorRow, kind: &str) -> String {
    let short = &row.id[..row.id.len().min(13)];
    match kind {
        "stalled" => format!(
            "no progress since {}. Check the source, or `mur monitor show {short} --history`",
            row.last_progress_at.to_rfc3339()
        ),
        "stalled_recovered" => "progress resumed — nothing to do".to_string(),
        "soft_deadline" => format!(
            "running longer than expected. `mur monitor show {short}` for evidence"
        ),
        "terminal" => format!("settled as {}. `mur monitor show {short}`", row.outcome.as_str()),
        "monitor_unhealthy" => format!(
            "the monitor cannot read its source ({} consecutive unknown checks) — this is a monitor problem, not a failure of the work. Check the credential reference and the source's reachability",
            row.unknown_streak
        ),
        // A hard-deadline exhaustion cannot be retried: `reactivate` leaves
        // `hard_reached` set (a passed deadline is a fact), so `mur monitor
        // retry` refuses. Saying otherwise sends the user at a wall.
        "exhausted" if row.hard_reached => format!(
            "stopped: its hard deadline ({}) passed. Retry cannot help — register a new monitor, or one with a longer `hard_deadline`, if this is still worth watching",
            row.spec.policy.hard_deadline
        ),
        "exhausted" => format!("stopped and needs a human. `mur monitor retry {short}` to re-enable"),
        other => format!("`mur monitor show {short}` ({other})"),
    }
}

pub fn render(row: &MonitorRow, event: &EventRow) -> Notification {
    // `actions taken` is required by §通知策略 but no executor exists until
    // the actions plan — the field renders as `—` rather than vanishing,
    // because its absence is information and the shape is the contract.
    let actions = "—";
    let next_check = if matches!(row.state, MonitorState::Completed | MonitorState::Exhausted) {
        "none (settled)".to_string()
    } else {
        row.next_check_at.to_rfc3339()
    };
    Notification {
        title: format!("MUR monitor: {} — {}", row.name, event.kind),
        body: format!(
            "{} · {} {}\noutcome: {} · actions: {} · next check: {}\nat {}",
            row.name,
            row.source_type.as_str(),
            row.reference,
            row.outcome.as_str(),
            actions,
            next_check,
            event.created_at.to_rfc3339(),
        ),
        next_step: next_step(row, &event.kind),
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor notify::`
Expected: 5 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor/src/notify.rs mur-monitor/src/lib.rs
git commit -m "feat(monitor): notifiable event kinds and the pure notification message"
```

---

### Task 2: The delivery queue

**Files:**
- Create: `mur-monitor/src/store/notify.rs`
- Modify: `mur-monitor/src/store/mod.rs` (`mod notify;` beside `mod lease;`/`mod observe;`, and `pub use notify::{Pending, DeliveryState};`)
- Test: `mur-monitor/src/store/notify.rs`

**Interfaces:**
- Consumes: `MonitorStore`, `MONITOR_COLS`, `row_to_monitor`, `ts`, `parse_ts`, `EventRow`, `notify::is_notifiable`, `backoff::unknown_delay`
- Produces:
  ```rust
  pub const DELIVERY_MAX_ATTEMPTS: u32 = 5;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum DeliveryState { Pending, Delivered, Failed }
  impl DeliveryState { pub fn as_str(self) -> &'static str; pub fn parse(s: &str) -> Option<Self> }
  pub struct Pending { pub event_id: i64, pub row: MonitorRow, pub event: EventRow, pub attempts: u32 }
  impl MonitorStore {
      pub fn pending_notifications(&self, channel: &str, now: DateTime<Utc>, max: usize) -> Result<Vec<Pending>>;
      pub fn mark_delivered(&self, event_id: i64, channel: &str, now: DateTime<Utc>) -> Result<()>;
      pub fn mark_delivery_failed(&self, event_id: i64, channel: &str, now: DateTime<Utc>) -> Result<DeliveryState>;
      pub fn delivery_states(&self, monitor_id: &str) -> Result<Vec<(i64, String, DeliveryState, u32)>>;
  }
  ```

**The keying rule, restated because it is the point of this task:** the row in `monitor_notifications` is keyed `"<event_id>:<channel>"` — **never** `(monitor, cycle, kind)`. See "The spec defect this plan corrects" above. One event row is one real transition; a second stall gets a second row and therefore a second notification.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{spec, t0};

    fn store_with_events(kinds: &[&str]) -> (tempfile::TempDir, MonitorStore, String) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec("k"), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        for k in kinds {
            s.append_event(&id, &cyc, k, serde_json::json!({}), false, t0()).unwrap();
        }
        (d, s, id)
    }

    #[test]
    fn only_notifiable_kinds_are_queued() {
        // `created` is written by `create` itself; the rest are ours.
        let (_d, s, _id) = store_with_events(&["observed", "stalled", "retried", "terminal"]);
        let p = s.pending_notifications("log", t0(), 10).unwrap();
        let kinds: Vec<_> = p.iter().map(|x| x.event.kind.as_str()).collect();
        assert_eq!(kinds, vec!["stalled", "terminal"], "bookkeeping must not queue");
    }

    #[test]
    fn a_delivered_event_is_not_offered_again() {
        let (_d, s, _id) = store_with_events(&["stalled"]);
        let p = s.pending_notifications("log", t0(), 10).unwrap();
        assert_eq!(p.len(), 1);
        s.mark_delivered(p[0].event_id, "log", t0()).unwrap();
        assert!(s.pending_notifications("log", t0(), 10).unwrap().is_empty());
    }

    #[test]
    fn channels_are_independent() {
        let (_d, s, _id) = store_with_events(&["stalled"]);
        let p = s.pending_notifications("log", t0(), 10).unwrap();
        s.mark_delivered(p[0].event_id, "log", t0()).unwrap();
        assert_eq!(
            s.pending_notifications("desktop", t0(), 10).unwrap().len(),
            1,
            "delivering to one channel must not silence another"
        );
    }

    /// The whole reason this plan does not key on (monitor, cycle, kind):
    /// `cycle_id` never rotates, so a second episode shares it.
    #[test]
    fn a_second_episode_of_the_same_kind_notifies_again() {
        let (_d, s, id) = store_with_events(&["stalled"]);
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        let first = s.pending_notifications("log", t0(), 10).unwrap();
        s.mark_delivered(first[0].event_id, "log", t0()).unwrap();

        // recovery, then a second stall — same monitor, same cycle, same kind
        s.append_event(&id, &cyc, "stalled_recovered", serde_json::json!({}), false, t0()).unwrap();
        s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0()).unwrap();

        let kinds: Vec<_> = s
            .pending_notifications("log", t0(), 10)
            .unwrap()
            .into_iter()
            .map(|p| p.event.kind)
            .collect();
        assert_eq!(kinds, vec!["stalled_recovered", "stalled"]);
    }

    #[test]
    fn a_failure_backs_off_then_parks_as_failed() {
        let (_d, s, _id) = store_with_events(&["stalled"]);
        let id0 = s.pending_notifications("log", t0(), 10).unwrap()[0].event_id;
        let mut state = DeliveryState::Pending;
        for i in 1..=DELIVERY_MAX_ATTEMPTS {
            state = s.mark_delivery_failed(id0, "log", t0()).unwrap();
            if i < DELIVERY_MAX_ATTEMPTS {
                assert_eq!(state, DeliveryState::Pending, "attempt {i} must stay retryable");
                // not due yet: the backoff pushes the next attempt out
                assert!(s.pending_notifications("log", t0(), 10).unwrap().is_empty());
            }
        }
        assert_eq!(state, DeliveryState::Failed);
        assert!(
            s.pending_notifications("log", t0() + chrono::Duration::days(1), 10).unwrap().is_empty(),
            "a parked failure must not be retried forever"
        );
    }

    #[test]
    fn max_bounds_a_drain() {
        let (_d, s, _id) = store_with_events(&["stalled", "soft_deadline", "terminal"]);
        assert_eq!(s.pending_notifications("log", t0(), 2).unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-monitor store::notify::`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
//! The delivery queue over `monitor_notifications` (created empty by the
//! read-only slice). One row per (event, channel).
//!
//! The key is the EVENT ROW ID, not `(monitor, cycle, kind)` as §通知策略
//! literally says. That wording predates the discovery that `cycle_id`
//! never rotates for a monitor: keying on it would silence every episode
//! after the first, which is the exact defect the read-only slice's final
//! review found and fixed in the events layer. Each event row is already
//! one real transition — every notifiable kind is written under a
//! transition guard — so one row is one notification.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};

use super::{parse_ts, row_to_monitor, ts, EventRow, MonitorRow, MonitorStore, MONITOR_COLS};
use crate::backoff::unknown_delay;

/// Attempts before a notification is parked. Retrying forever is a busy
/// loop with a friendly name.
pub const DELIVERY_MAX_ATTEMPTS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    Pending,
    Delivered,
    Failed,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryState::Pending => "pending",
            DeliveryState::Delivered => "delivered",
            DeliveryState::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(DeliveryState::Pending),
            "delivered" => Some(DeliveryState::Delivered),
            "failed" => Some(DeliveryState::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pending {
    pub event_id: i64,
    pub row: MonitorRow,
    pub event: EventRow,
    pub attempts: u32,
}

fn key(event_id: i64, channel: &str) -> String {
    format!("{event_id}:{channel}")
}

impl MonitorStore {
    /// Notifiable events with no delivered/failed row for this channel and
    /// whose retry time has arrived. Oldest first — a user reading a backlog
    /// wants it in the order it happened.
    pub fn pending_notifications(
        &self,
        channel: &str,
        now: DateTime<Utc>,
        max: usize,
    ) -> Result<Vec<Pending>> {
        let placeholders = crate::notify::NOTIFIABLE
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT e.id, e.monitor_id, e.cycle_id, e.kind, e.payload, e.created_at, \
                    n.delivery_state, n.updated_at, {MONITOR_COLS} \
             FROM monitor_events e \
             JOIN monitors m ON m.id = e.monitor_id \
             LEFT JOIN monitor_notifications n ON n.event_key = (e.id || ':' || ?1) \
             WHERE e.kind IN ({placeholders}) \
               AND (n.delivery_state IS NULL OR n.delivery_state = 'pending') \
             ORDER BY e.id ASC LIMIT ?2"
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(channel.to_string())];
        args.push(Box::new(max as i64));
        for k in crate::notify::NOTIFIABLE {
            args.push(Box::new(k.to_string()));
        }
        // rusqlite binds by position: ?1 channel, ?2 max, then the kinds.
        // `params_from_iter` walks them in order, so build the vec in the
        // same order the SQL numbers them.
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())), |r| {
            let payload: String = r.get(4)?;
            let attempts_at: Option<String> = r.get(7)?;
            Ok((
                r.get::<_, i64>(0)?,
                EventRow {
                    cycle_id: r.get(2)?,
                    kind: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                    created_at: parse_ts(&r.get::<_, String>(5)?),
                },
                attempts_at,
                row_to_monitor_offset(r, 8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (event_id, event, _seen, m) = row?;
            let attempts = self.delivery_attempts(event_id, channel)?;
            if let Some(next) = self.delivery_next_attempt_at(event_id, channel)?
                && next > now
            {
                continue;
            }
            out.push(Pending { event_id, row: m, event, attempts });
        }
        Ok(out)
    }

    pub fn mark_delivered(&self, event_id: i64, channel: &str, now: DateTime<Utc>) -> Result<()> {
        self.conn().execute(
            "INSERT INTO monitor_notifications (event_key, monitor_id, channel, delivery_state, updated_at) \
             VALUES (?1, (SELECT monitor_id FROM monitor_events WHERE id = ?2), ?3, 'delivered', ?4) \
             ON CONFLICT(event_key) DO UPDATE SET delivery_state = 'delivered', updated_at = ?4",
            params![key(event_id, channel), event_id, channel, ts(now)],
        )?;
        Ok(())
    }

    /// Records a failed attempt and returns the resulting state: `Pending`
    /// while attempts remain (with the next attempt pushed out by the
    /// unknown-backoff curve), `Failed` once the ceiling is hit.
    pub fn mark_delivery_failed(
        &self,
        event_id: i64,
        channel: &str,
        now: DateTime<Utc>,
    ) -> Result<DeliveryState> {
        let attempts = self.delivery_attempts(event_id, channel)? + 1;
        let state = if attempts >= DELIVERY_MAX_ATTEMPTS {
            DeliveryState::Failed
        } else {
            DeliveryState::Pending
        };
        let next = now
            + chrono::Duration::from_std(unknown_delay(attempts.saturating_sub(1)))
                .unwrap_or_else(|_| chrono::Duration::seconds(60));
        // `updated_at` doubles as "not before": a pending row is only
        // offered again once that instant has passed.
        self.conn().execute(
            "INSERT INTO monitor_notifications (event_key, monitor_id, channel, delivery_state, updated_at) \
             VALUES (?1, (SELECT monitor_id FROM monitor_events WHERE id = ?2), ?3, ?4, ?5) \
             ON CONFLICT(event_key) DO UPDATE SET delivery_state = ?4, updated_at = ?5",
            params![key(event_id, channel), event_id, channel, state.as_str(), ts(next)],
        )?;
        self.bump_delivery_attempts(event_id, channel, attempts)?;
        Ok(state)
    }

    pub fn delivery_states(
        &self,
        monitor_id: &str,
    ) -> Result<Vec<(i64, String, DeliveryState, u32)>> {
        let mut stmt = self.conn().prepare(
            "SELECT e.id, n.channel, n.delivery_state FROM monitor_notifications n \
             JOIN monitor_events e ON (e.id || ':' || n.channel) = n.event_key \
             WHERE n.monitor_id = ?1 ORDER BY e.id ASC",
        )?;
        let rows = stmt.query_map([monitor_id], |r| {
            let st: String = r.get(2)?;
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                DeliveryState::parse(&st).unwrap_or(DeliveryState::Pending),
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ch, st) = row?;
            let a = self.delivery_attempts(id, &ch)?;
            out.push((id, ch, st, a));
        }
        Ok(out)
    }
}
```

**Two helpers this code needs that the snippet above references** — implement them in the same file. `monitor_notifications` has no `attempts` column and this plan does not alter the schema (see Global Constraints), so attempts are derived rather than stored: keep a private `attempts` count in a small side table created with `CREATE TABLE IF NOT EXISTS monitor_notification_attempts (event_key TEXT PRIMARY KEY, n INTEGER NOT NULL)` inside this module's own `ensure_tables(&self)` called at the top of each public method, or — simpler and preferred — encode the attempt count in `delivery_state` as `pending:N`. **Pick one, and say which in your report.** If you choose `pending:N`, `DeliveryState::parse` must accept it and `as_str` must round-trip; update the Task-2 tests' expectations accordingly and keep `delivery_states` returning the numeric count.

`row_to_monitor_offset(r, 8)` is `row_to_monitor` reading from a column offset (the join puts the monitor's columns after the event's). Either add a `pub(crate) fn row_to_monitor_at(r: &Row, base: usize)` to `store/mod.rs` and have the existing `row_to_monitor` call it with `0`, or select the monitor separately with `self.get(&monitor_id)?`. The second is one extra query per pending row and is bounded by `max`; prefer it for clarity unless the join proves necessary.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo nextest run -p mur-monitor store::notify::`
Expected: 6 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-monitor --all-targets -- -D warnings
git add mur-monitor/src/store
git commit -m "feat(monitor): notification delivery queue keyed on the event row"
```

---

### Task 3: The `Channel` trait and the always-on log channel

**Files:**
- Create: `mur-core/src/monitor/notify/mod.rs`
- Create: `mur-core/src/monitor/notify/log.rs`
- Modify: `mur-core/src/monitor/mod.rs` (`pub mod notify;`)
- Test: both files

**Interfaces:**
- Consumes: `mur_monitor::notify::Notification`
- Produces:
  ```rust
  pub trait Channel: Send + Sync {
      fn name(&self) -> &'static str;
      fn deliver(&self, n: &Notification) -> Result<(), String>;
  }
  pub struct ChannelRegistry;
  impl ChannelRegistry {
      pub fn new() -> Self;
      pub fn register(&mut self, c: Box<dyn Channel>);
      pub fn iter(&self) -> impl Iterator<Item = &dyn Channel>;
  }
  pub struct LogChannel;   // in log.rs, name() == "log"
  ```

- [ ] **Step 1: Write the failing tests**

```rust
// mod.rs
#[cfg(test)]
mod tests {
    use super::*;
    use mur_monitor::notify::Notification;

    struct Fake(&'static str, std::sync::atomic::AtomicUsize);
    impl Channel for Fake {
        fn name(&self) -> &'static str { self.0 }
        fn deliver(&self, _n: &Notification) -> Result<(), String> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn registry_iterates_in_registration_order() {
        let mut r = ChannelRegistry::new();
        r.register(Box::new(Fake("a", Default::default())));
        r.register(Box::new(Fake("b", Default::default())));
        assert_eq!(r.iter().map(|c| c.name()).collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn a_channel_that_errors_returns_its_reason() {
        struct Broken;
        impl Channel for Broken {
            fn name(&self) -> &'static str { "broken" }
            fn deliver(&self, _n: &Notification) -> Result<(), String> { Err("no display".into()) }
        }
        assert_eq!(Broken.deliver(&n()).unwrap_err(), "no display");
    }

    fn n() -> Notification {
        Notification {
            title: "MUR monitor: t — stalled".into(),
            body: "b".into(),
            next_step: "s".into(),
        }
    }
}
```

```rust
// log.rs
#[cfg(test)]
mod tests {
    use super::*;
    use mur_monitor::notify::Notification;

    #[test]
    fn the_log_channel_is_named_log_and_never_fails() {
        // It must never fail: it is the channel of last resort, the one that
        // records a notable event even when every other channel is off.
        let c = LogChannel;
        assert_eq!(c.name(), "log");
        assert!(c.deliver(&Notification {
            title: "MUR monitor: t — stalled".into(),
            body: "body".into(),
            next_step: "step".into(),
        }).is_ok());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::notify`
Expected: compile error.

- [ ] **Step 3: Implement**

```rust
// mur-core/src/monitor/notify/mod.rs
//! Delivery channels. The queue and the message live in `mur-monitor`
//! (below this crate, deliberately — it must not spawn processes); the
//! channels live here because they do.

pub mod desktop;
pub mod log;

use mur_monitor::notify::Notification;

pub trait Channel: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Err(reason)` is recorded and retried on the queue's backoff. A
    /// channel must not panic and must not block for long: the drain runs
    /// on the daemon's tick.
    fn deliver(&self, n: &Notification) -> Result<(), String>;
}

#[derive(Default)]
pub struct ChannelRegistry {
    channels: Vec<Box<dyn Channel>>,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, c: Box<dyn Channel>) {
        self.channels.push(c);
    }
    pub fn iter(&self) -> impl Iterator<Item = &dyn Channel> {
        self.channels.iter().map(|b| b.as_ref())
    }
}
```

```rust
// mur-core/src/monitor/notify/log.rs
//! The channel of last resort: always registered, cannot be switched off.
//! A monitor that went unhealthy must leave a trace somewhere even when a
//! user has turned every other channel off, and the daemon log is where an
//! operator already looks.

use mur_monitor::notify::Notification;

use super::Channel;

pub struct LogChannel;

impl Channel for LogChannel {
    fn name(&self) -> &'static str {
        "log"
    }

    fn deliver(&self, n: &Notification) -> Result<(), String> {
        tracing::info!(
            title = %n.title,
            next_step = %n.next_step,
            "{}",
            n.body.replace('\n', " · ")
        );
        Ok(())
    }
}
```

- [ ] **Step 4: Run to verify it passes** — same command; 3 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/monitor
git commit -m "feat(monitor): notification Channel trait and the always-on log channel"
```

---

### Task 4: The desktop channel

**Files:**
- Create: `mur-core/src/monitor/notify/desktop.rs`
- Test: same file

**Interfaces:**
- Produces: `pub struct DesktopChannel;` (`name() == "desktop"`), `pub fn script(title: &str, message: &str) -> String`

**Read first:** `mur-core/src/cmd/agent/cli/notify.rs` — it already does exactly this shape (osascript on macOS, `notify-send` on Linux, no-op elsewhere) with the escaping pulled out as a pure function so it can be unit-tested. Follow it; do not invent a second escaping scheme.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_backslashes_are_escaped_for_osascript() {
        let s = script("MUR monitor: a\"b", "c\\d");
        assert!(s.contains("a\\\"b"), "{s}");
        assert!(s.contains("c\\\\d"), "{s}");
    }

    #[test]
    fn a_newline_in_the_body_does_not_break_the_script() {
        // `render` puts newlines in `body`; an unescaped one would truncate
        // the osascript line and silently deliver half a message.
        let s = script("t", "line one\nline two");
        assert!(!s.contains('\n'), "the script must be a single line: {s:?}");
        assert!(s.contains("line one") && s.contains("line two"), "{s}");
    }

    #[test]
    fn the_channel_is_named_desktop() {
        assert_eq!(DesktopChannel.name(), "desktop");
    }
}
```

- [ ] **Step 2: Run to verify it fails** — compile error.

- [ ] **Step 3: Implement**

```rust
//! Desktop notification, following `cmd/agent/cli/notify.rs`'s established
//! shape: a pure script builder so the escaping is unit-tested, and a
//! spawn-and-ignore delivery. Opt-in (`notifications.desktop`, default
//! false): a background daemon that starts popping OS notifications the
//! moment a user upgrades is a hostile default.

use mur_monitor::notify::Notification;

use super::Channel;

/// One osascript line, with quotes, backslashes and newlines escaped.
/// Newlines matter: `render`'s body is multi-line, and an unescaped one
/// truncates the script — delivering half a message with no error.
pub fn script(title: &str, message: &str) -> String {
    fn esc(s: &str) -> String {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " · ")
    }
    format!(
        "display notification \"{}\" with title \"{}\"",
        esc(message),
        esc(title)
    )
}

pub struct DesktopChannel;

impl Channel for DesktopChannel {
    fn name(&self) -> &'static str {
        "desktop"
    }

    fn deliver(&self, n: &Notification) -> Result<(), String> {
        // Lead with the next step: a banner truncates, and the step is the
        // part that is worth the interruption.
        let body = format!("{} — {}", n.next_step, n.body.replace('\n', " · "));
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("osascript")
                .args(["-e", &script(&n.title, &body)])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("osascript: {e}"))
        }
        #[cfg(target_os = "linux")]
        {
            std::process::Command::new("notify-send")
                .args([&n.title, &body])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("notify-send: {e}"))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = body;
            Err("no desktop notification backend on this platform".into())
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes** — 3 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/monitor/notify/desktop.rs
git commit -m "feat(monitor): opt-in desktop notification channel"
```

---

### Task 5: Config

**Files:**
- Modify: `mur-common/src/config.rs` (a `NotificationsConfig` struct plus a `notifications` field on `Config`)
- Modify: `mur-core/src/monitor/notify/mod.rs` (a `registry_from_config` builder)
- Test: `mur-common/src/config.rs`, `mur-core/src/monitor/notify/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  // mur-common
  pub struct NotificationsConfig { pub desktop: bool }   // Default: desktop = false
  // on Config: #[serde(default)] pub notifications: NotificationsConfig
  // mur-core
  pub fn registry_from_config(cfg: &NotificationsConfig) -> ChannelRegistry
  ```

- [ ] **Step 1: Write the failing tests**

```rust
// mur-common/src/config.rs tests
#[test]
fn notifications_default_to_log_only() {
    let c: Config = serde_yaml::from_str("{}").unwrap();
    assert!(!c.notifications.desktop, "desktop must be opt-in");
}

#[test]
fn an_existing_config_without_the_block_still_parses() {
    // Every user upgrading has a config.yaml with no `notifications:` key.
    let c: Config = serde_yaml::from_str("retrieval:\n  min_score: 0.42\n").unwrap();
    assert!(!c.notifications.desktop);
}
```

```rust
// mur-core/src/monitor/notify/mod.rs tests
#[test]
fn log_is_always_registered_and_desktop_only_when_enabled() {
    let off = registry_from_config(&NotificationsConfig { desktop: false });
    assert_eq!(off.iter().map(|c| c.name()).collect::<Vec<_>>(), vec!["log"]);

    let on = registry_from_config(&NotificationsConfig { desktop: true });
    assert_eq!(on.iter().map(|c| c.name()).collect::<Vec<_>>(), vec!["log", "desktop"]);
}
```

- [ ] **Step 2: Run to verify they fail**

```
cargo nextest run -p mur-common notifications
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core monitor::notify
```
Expected: compile errors.

- [ ] **Step 3: Implement**

```rust
// mur-common/src/config.rs — beside the other config blocks
/// Which notification channels the durable monitor may use. `log` is not
/// listed: it is always on and cannot be disabled, because a notable event
/// must leave a trace somewhere even when a user has turned everything off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotificationsConfig {
    /// OS desktop notification. Opt-in: a background daemon that starts
    /// popping banners on upgrade is a hostile default.
    #[serde(default)]
    pub desktop: bool,
}
```
Add to `Config`: `#[serde(default)] pub notifications: NotificationsConfig,`

```rust
// mur-core/src/monitor/notify/mod.rs
use mur_common::config::NotificationsConfig;

pub fn registry_from_config(cfg: &NotificationsConfig) -> ChannelRegistry {
    let mut r = ChannelRegistry::new();
    r.register(Box::new(log::LogChannel));
    if cfg.desktop {
        r.register(Box::new(desktop::DesktopChannel));
    }
    r
}
```

- [ ] **Step 4: Run to verify they pass** — 4 PASS across the two crates.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-common -p mur-core --all-targets -- -D warnings
git add mur-common/src/config.rs mur-core/src/monitor/notify/mod.rs
git commit -m "feat(monitor): notifications config block, desktop opt-in"
```

---

### Task 6: Drain on the daemon tick

**Files:**
- Modify: `mur-core/src/monitor/service.rs` (`drain_notifications`)
- Modify: `mur-daemon/src/monitor_tick.rs` (call it after `tick_once`)
- Test: `mur-core/src/monitor/service.rs`

**Interfaces:**
- Consumes: `MonitorStore::{open_existing, pending_notifications, mark_delivered, mark_delivery_failed}`, `mur_monitor::notify::render`, `registry_from_config`, `mur_common::config::Config`
- Produces:
  ```rust
  pub const DRAIN_MAX_PER_TICK: usize = 20;
  #[derive(Debug, Default, PartialEq, Eq)] pub struct DrainReport { pub delivered: usize, pub failed: usize, pub parked: usize }
  pub fn drain_notifications(mur_home: &Path, now: DateTime<Utc>) -> anyhow::Result<DrainReport>
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_home_with_no_store_drains_nothing_and_creates_nothing() {
    // Same rule as the tick: a user who has never run `mur monitor`
    // acquires no database. Assert on the filesystem, not the report — an
    // empty report comes back either way.
    let d = tempfile::tempdir().unwrap();
    let dir = mur_monitor::store::db_dir(d.path());
    assert!(!dir.exists());
    let r = drain_notifications(d.path(), Utc::now()).unwrap();
    assert_eq!(r, DrainReport::default());
    assert!(!dir.exists(), "draining must not create the store");
}

#[test]
fn a_notifiable_event_is_delivered_once_and_not_again() {
    let d = tempfile::tempdir().unwrap();
    let s = mur_monitor::store::MonitorStore::open(d.path()).unwrap();
    let id = s.create(&spec(), t0(), None).unwrap().id;
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;
    s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0()).unwrap();
    drop(s);

    let first = drain_notifications(d.path(), t0()).unwrap();
    assert_eq!(first.delivered, 1);
    let second = drain_notifications(d.path(), t0()).unwrap();
    assert_eq!(second.delivered, 0, "a delivered notification must not repeat");
}

#[test]
fn bookkeeping_events_are_never_delivered() {
    let d = tempfile::tempdir().unwrap();
    let s = mur_monitor::store::MonitorStore::open(d.path()).unwrap();
    let id = s.create(&spec(), t0(), None).unwrap().id;  // writes `created`
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;
    s.append_event(&id, &cyc, "lease_recovered", serde_json::json!({}), false, t0()).unwrap();
    drop(s);
    assert_eq!(drain_notifications(d.path(), t0()).unwrap(), DrainReport::default());
}
```

- [ ] **Step 2: Run to verify they fail** — compile error.

- [ ] **Step 3: Implement**

```rust
/// Notifications delivered per tick. Bounded so a backlog after downtime
/// spreads across ticks instead of firing a hundred banners at once.
pub const DRAIN_MAX_PER_TICK: usize = 20;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub delivered: usize,
    pub failed: usize,
    pub parked: usize,
}

/// Deliver what the tick recorded. Never creates a store (a user who has
/// never run `mur monitor` acquires nothing) and never fails the caller: a
/// delivery problem is recorded on the queue and retried, per spec §錯誤處理
/// ("notification failure: 不回滾已完成 action").
pub fn drain_notifications(mur_home: &Path, now: DateTime<Utc>) -> Result<DrainReport> {
    let Some(store) = MonitorStore::open_existing(mur_home)? else {
        return Ok(DrainReport::default());
    };
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let registry = super::notify::registry_from_config(&cfg.notifications);
    let mut rep = DrainReport::default();
    for channel in registry.iter() {
        for p in store.pending_notifications(channel.name(), now, DRAIN_MAX_PER_TICK)? {
            let n = mur_monitor::notify::render(&p.row, &p.event);
            match channel.deliver(&n) {
                Ok(()) => {
                    store.mark_delivered(p.event_id, channel.name(), now)?;
                    rep.delivered += 1;
                }
                Err(reason) => {
                    let state = store.mark_delivery_failed(p.event_id, channel.name(), now)?;
                    tracing::warn!(
                        channel = channel.name(),
                        event_id = p.event_id,
                        %reason,
                        "monitor notification delivery failed"
                    );
                    match state {
                        mur_monitor::store::DeliveryState::Failed => rep.parked += 1,
                        _ => rep.failed += 1,
                    }
                }
            }
        }
    }
    Ok(rep)
}
```

In `mur-daemon/src/monitor_tick.rs`, after the `tick_once` match and before the sleep:

```rust
        match service::drain_notifications(mur_home, Utc::now()) {
            Ok(d) if d.delivered + d.failed + d.parked > 0 => tracing::info!(
                delivered = d.delivered,
                failed = d.failed,
                parked = d.parked,
                "monitor notifications"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "monitor notification drain failed"),
        }
```

- [ ] **Step 4: Run to verify they pass** — 3 PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core -p mur-daemon --all-targets -- -D warnings
git add mur-core/src/monitor/service.rs mur-daemon/src/monitor_tick.rs
git commit -m "feat(monitor): drain notifications on the daemon tick"
```

---

### Task 7: Delivery state in `show`, plus docs

**Files:**
- Modify: `mur-core/src/cmd/monitor.rs` (`show` renders delivery state)
- Modify: `CLAUDE.md` (the `mur monitor` bullet), `README.md` (the "Durable monitors" section)
- Test: `mur-core/src/cmd/monitor_tests.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn show_renders_notification_delivery_state() {
    // spec §錯誤處理: a delivery failure must be visible in the CLI.
    let d = home();
    go(d.path(), MonitorAction::Add { file: spec_file(d.path(), "mur_run", "run-1"), started_at: None }).unwrap();
    let s = MonitorStore::open(d.path()).unwrap();
    let id = s.list(&ListFilter::default()).unwrap()[0].id.clone();
    let cyc = s.get(&id).unwrap().unwrap().cycle_id;
    s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0()).unwrap();
    let ev = s.pending_notifications("log", t0(), 1).unwrap();
    s.mark_delivery_failed(ev[0].event_id, "log", t0()).unwrap();

    let out = go(d.path(), MonitorAction::Show { id: id.clone(), history: false }).unwrap();
    assert!(out.contains("notifications:"), "{out}");
    assert!(out.contains("log") && out.contains("pending"), "{out}");
}
```

- [ ] **Step 2: Run to verify it fails.**

- [ ] **Step 3: Implement** — in `show`, after the lease line and before `recent observations`:

```rust
    let deliveries = store.delivery_states(id)?;
    if !deliveries.is_empty() {
        writeln!(out, "  notifications:")?;
        for (event_id, channel, state, attempts) in deliveries {
            writeln!(
                out,
                "    event {event_id}  {channel:<8} {}{}",
                state.as_str(),
                if attempts > 0 { format!(" ({attempts} attempt(s))") } else { String::new() }
            )?;
        }
    }
```

- [ ] **Step 4: Run to verify it passes.**

- [ ] **Step 5: Docs**

`CLAUDE.md`, extend the existing `mur monitor` bullet with one sentence:
> Notable events (stalled, soft deadline, terminal, monitor unhealthy, exhausted) are delivered once each to the daemon log, and to an OS notification when `notifications.desktop: true` in `config.yaml`; routine polling stays silent, and `mur monitor show` reports each notification's delivery state.

`README.md`, in the "Durable monitors" section after the CLI block:
> When something notable happens — a monitor stalls, crosses its soft deadline, settles, goes unhealthy, or gives up — MUR says so once, in the daemon log and (opt-in, `notifications.desktop: true`) as a desktop notification. Routine polling says nothing. Each message names the monitor, its source, what is known, when the next check is, and the single next step.

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p mur-core --all-targets -- -D warnings
git add mur-core/src/cmd/monitor.rs mur-core/src/cmd/monitor_tests.rs CLAUDE.md README.md
git commit -m "feat(monitor): show notification delivery state; document the notifier"
```

---

## Self-review

**Spec coverage** (§通知策略 clause → task):

| Clause | Task |
|---|---|
| 正常 polling 不通知 | 1 (`NOTIFIABLE` excludes every routine kind; tested both ways) |
| 首次進入 stalled + 一則 recovery | 1, 2 (transition guards wrote the rows; the queue keys on the row) |
| 首次跨過 soft deadline | 1 |
| 需要 approval | **out of scope** — no approval exists until the actions plan; `NOTIFIABLE` gains the kind then |
| 補救失敗或達上限 | **out of scope** — same reason; `exhausted` is covered now |
| 終態完成結算 | 1 (`terminal`) |
| monitor 自身 unknown / credential / adapter 壞掉 | 1 (`monitor_unhealthy`) |
| 每種 event 去重，狀態未改變不重複吵人 | 2 — **deliberately keyed on the event row id, not (monitor, cycle, kind); see the correction section** |
| 訊息含 name / reference / outcome / actions / next check / 單一步驟 | 1 (`render`, asserted field by field) |
| notification failure: 退避重送 + CLI 顯示 delivery state | 2 (backoff + park), 6 (never fails the tick), 7 (`show`) |
| §可觀測性 metrics | **out of scope, stated in the header** — no consumer until a dashboard exists |

**Placeholder scan:** one deliberate open decision, in Task 2 — whether attempts live in a side table or encoded in `delivery_state`. It is marked, both options are spelled out, and the implementer must state which it chose. Everything else is concrete.

**Type consistency:** `Notification{title, body, next_step}` (1) consumed by 3, 4, 6. `Pending{event_id, row, event, attempts}` (2) consumed by 6, 7. `DeliveryState` (2) consumed by 6, 7. `Channel::{name, deliver}` (3) implemented by 4, iterated by 6. `NotificationsConfig{desktop}` (5) consumed by 6.

**Known ceilings, named:** `pending_notifications` runs one query per channel per tick (two channels today); the drain is bounded at `DRAIN_MAX_PER_TICK = 20` so a backlog spreads over ticks rather than arriving at once; a desktop notification is fire-and-forget, so "delivered" means "the spawn succeeded", not that a human saw it.
