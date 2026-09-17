//! The `monitor_actions` table (spec §冪等與事件紀錄): claim before you act.
//!
//! `action_key` (`crate::action::action_key`) is the PRIMARY KEY, so "has
//! this side effect already run?" is answered by the database refusing a
//! duplicate INSERT, never by a SELECT followed by an INSERT in Rust — two
//! daemons, or one daemon restarting mid-action, would both pass a
//! check-then-act.
//!
//! `claim_action` returning `Ok(false)` is the ordinary path after a
//! restart, not an error: the terminal is being replayed and the action
//! already has a row.
//!
//! Unlike `store/lease.rs`'s `claim_due` (a SELECT that decides which rows
//! to update, then an UPDATE — a genuine read-then-write, hence the explicit
//! `BEGIN IMMEDIATE` there), every write below is a single SQL statement:
//! `claim_action` is one `INSERT OR IGNORE`, `finish_action` and
//! `block_action` are each one `UPDATE` whose new value (`attempt + 1`,
//! `state = ...`) is computed by SQLite itself, not read back into Rust
//! first. A single statement is already one atomic unit under SQLite's
//! default autocommit, so wrapping it in its own transaction would add
//! nothing — the module doc on `lease.rs` explains the *deferred-transaction
//! snapshot* hazard that `BEGIN IMMEDIATE` fixes, and that hazard requires a
//! read whose result the same transaction later acts on, which does not
//! happen here.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_common::hitl::RiskTier;

use super::{MonitorStore, parse_ts, ts};
use crate::action::ActionState;

#[derive(Debug, Clone)]
pub struct ActionRow {
    pub action_key: String,
    pub monitor_id: String,
    pub cycle_id: String,
    pub risk: RiskTier,
    pub approval_id: Option<String>,
    pub state: ActionState,
    pub attempt: u32,
    pub result: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// How much of an action's result is kept in history. Redaction runs
/// BEFORE this cut, never after: a fixed-length secret pattern straddling
/// the cut survives unmatched if it is cut first and redacted second — the
/// shipped observation path had exactly that bug the other way round.
const RESULT_MAX_CHARS: usize = 400;

fn store_result(raw: &str) -> String {
    let redacted = mur_common::redact::redact_secrets(raw);
    redacted.chars().take(RESULT_MAX_CHARS).collect()
}

/// `RiskTier` round-trips through its own `Serialize`/`Deserialize`
/// (kebab-case) rather than a hand-written match here, so a tier renamed or
/// added in `mur_common::hitl` cannot silently drift out of sync with what
/// this table stores.
fn risk_to_sql(risk: RiskTier) -> Result<String> {
    match serde_json::to_value(risk).context("serialize risk tier")? {
        serde_json::Value::String(s) => Ok(s),
        other => anyhow::bail!("RiskTier serialized to non-string {other:?}"),
    }
}

fn risk_from_sql(s: &str) -> Option<RiskTier> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

impl MonitorStore {
    /// `Ok(true)` when this call created the row — the caller owns the
    /// side effect and must run it. `Ok(false)` when a row already exists
    /// under this key: the caller must NOT run the side effect again.
    pub fn claim_action(
        &self,
        action_key: &str,
        monitor_id: &str,
        cycle_id: &str,
        risk: RiskTier,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT OR IGNORE INTO monitor_actions \
             (action_key, monitor_id, cycle_id, risk, state, attempt, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
            rusqlite::params![
                action_key,
                monitor_id,
                cycle_id,
                risk_to_sql(risk)?,
                ActionState::Claimed.as_str(),
                ts(now),
            ],
        )?;
        Ok(n == 1)
    }

    /// Terminal for this action: `Done` or `Failed`. The result is
    /// redacted, then truncated — never the other order (see module doc).
    /// `created_at` is when the action was CLAIMED and does not move here;
    /// this table has no separate finish-time column.
    pub fn finish_action(&self, action_key: &str, state: ActionState, result: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE monitor_actions SET state = ?1, result = ?2 WHERE action_key = ?3",
            rusqlite::params![state.as_str(), store_result(result), action_key],
        )?;
        Ok(())
    }

    /// Parked awaiting a human, recording which approval request is
    /// outstanding. NOT a failure — `pending_actions` still returns it, so a
    /// later tick (or a late-arriving approval) picks it back up.
    ///
    /// `attempt` counts times this action was PARKED ON SOMETHING NEW, not
    /// times the drain looked at it. Re-parking a row that is already
    /// `blocked` on the same `approval_id` leaves it alone: the drain
    /// re-gates every blocked row on every tick and the gate correctly hands
    /// back the existing request (`hitl::gate`'s `Prior::Pending` arm), so an
    /// unconditional `attempt + 1` reached 240 after an hour at the daemon's
    /// 15 s cadence and `mur monitor show` reported thousands of "attempts"
    /// for an action that was never attempted — plus one pointless `UPDATE`
    /// per parked action per tick, indefinitely. Same principle the
    /// remediation budget already follows: waiting for a human is not
    /// remediating.
    ///
    /// The `CASE` reads the row's PRE-update `state`/`approval_id` — in
    /// SQLite every `SET` expression sees the original row — so a first block
    /// (state `claimed`, `approval_id` NULL) still counts, and so does being
    /// parked on a genuinely different request.
    pub fn block_action(&self, action_key: &str, approval_id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE monitor_actions SET state = ?1, approval_id = ?2, \
             attempt = attempt + CASE WHEN state = ?1 AND approval_id = ?2 THEN 0 ELSE 1 END \
             WHERE action_key = ?3",
            rusqlite::params![ActionState::Blocked.as_str(), approval_id, action_key],
        )?;
        Ok(())
    }

    /// Record why an action could not be processed this tick, WITHOUT
    /// settling it: `state` is untouched, so `pending_actions` returns the
    /// row again and the next tick retries it. This is spec §錯誤處理's
    /// 「單一單位失敗要記錄並重試，不得向上傳播」 for the one failure that is
    /// neither the executor's nor a decision — the approval gate itself
    /// erroring (an unreadable channel event file, a signing-key failure in
    /// `append_as_writer`). `finish_action` is the wrong tool for it: that
    /// one is terminal by contract, and a transient channel fault must not
    /// permanently fail a remedy.
    /// Returns the new consecutive-failure total so the caller can decide
    /// whether to keep retrying. Counting is the point: without it the row
    /// stays `Claimed` and is retried every tick forever, holding one of the
    /// slots `pending_actions` hands back.
    pub fn record_action_error(&self, action_key: &str, reason: &str) -> Result<u32> {
        self.conn().execute(
            "UPDATE monitor_actions SET result = ?1, gate_errors = gate_errors + 1 \
             WHERE action_key = ?2",
            rusqlite::params![store_result(reason), action_key],
        )?;
        let n: i64 = self.conn().query_row(
            "SELECT gate_errors FROM monitor_actions WHERE action_key = ?1",
            [action_key],
            |r| r.get(0),
        )?;
        Ok(n as u32)
    }

    /// Clear the consecutive-failure count. Called whenever the gate
    /// actually answers — "consecutive" is only meaningful if a success
    /// resets it, and a gate that answers has not failed.
    pub fn clear_action_errors(&self, action_key: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE monitor_actions SET gate_errors = 0 WHERE action_key = ?1 AND gate_errors != 0",
            [action_key],
        )?;
        Ok(())
    }

    /// Everything recorded for one monitor, oldest first.
    pub fn actions_for(&self, monitor_id: &str) -> Result<Vec<ActionRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT action_key, monitor_id, cycle_id, risk, approval_id, state, attempt, result, created_at \
             FROM monitor_actions WHERE monitor_id = ?1 ORDER BY created_at ASC, action_key ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![monitor_id], row_to_action)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
            .map_err(Into::into)
    }

    /// Actions still owed work: `claimed` (never started) or `blocked`
    /// (waiting on a human, and a later approval may have landed). `now` is
    /// part of the interface the daemon poll loop dials, kept here for
    /// forward compatibility even though this slice has no time-based
    /// filter to apply (there is no retry-delay column on this table yet).
    ///
    /// Two limits, not one (M2, whole-branch review): a single combined
    /// `LIMIT` ordered oldest-first lets `blocked` rows alone fill the
    /// window, and a `blocked` row never leaves it until answered — so past
    /// `claimed_max + blocked_max` (formerly one shared cap) parked
    /// actions, the newest ones are excluded from every future call and an
    /// approval landing on one of them is never looked at again. Re-checking
    /// a parked approval is a cheap gate lookup, not real work (the same
    /// principle `block_action`'s doc already applies to `attempt`), so it
    /// must not compete with fresh `claimed` rows for the same window —
    /// hence its own, separate limit. `claimed` and `blocked` rows are
    /// queried and ordered independently, then concatenated: callers that
    /// need to bound `claimed` and `blocked` work separately (the drain)
    /// can tell them apart by `ActionRow::state`; callers that do not
    /// (existing tests) can pass the same value for both.
    pub fn pending_actions(
        &self,
        now: DateTime<Utc>,
        claimed_max: usize,
        blocked_max: usize,
    ) -> Result<Vec<ActionRow>> {
        let _ = now;
        let mut rows = self.actions_in_state(ActionState::Claimed, claimed_max)?;
        rows.extend(self.actions_in_state(ActionState::Blocked, blocked_max)?);
        Ok(rows)
    }

    fn actions_in_state(&self, state: ActionState, max: usize) -> Result<Vec<ActionRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT action_key, monitor_id, cycle_id, risk, approval_id, state, attempt, result, created_at \
             FROM monitor_actions WHERE state = ?1 \
             ORDER BY created_at ASC, action_key ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![state.as_str(), max as i64], row_to_action)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map(|v| v.into_iter().flatten().collect())
            .map_err(Into::into)
    }
}

/// `None` for a row whose `state` or `risk` no longer parses — a forward
/// compatibility hole, not a crash: a newer build's state name must not
/// panic an older one mid-tick.
fn row_to_action(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ActionRow>> {
    let state: String = r.get(5)?;
    let risk: String = r.get(3)?;
    let (Some(state), Some(risk)) = (ActionState::parse(&state), risk_from_sql(&risk)) else {
        return Ok(None);
    };
    let created_at: String = r.get(8)?;
    Ok(Some(ActionRow {
        action_key: r.get(0)?,
        monitor_id: r.get(1)?,
        cycle_id: r.get(2)?,
        risk,
        approval_id: r.get(4)?,
        state,
        attempt: r.get::<_, i64>(6)? as u32,
        result: r.get(7)?,
        created_at: parse_ts(&created_at),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::action_key;
    use crate::store::tests::{spec, t0};

    fn fixture() -> (tempfile::TempDir, MonitorStore, String, String) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let created = s.create(&spec("k"), t0(), None).unwrap();
        let cyc = s.get(&created.id).unwrap().unwrap().cycle_id;
        (d, s, created.id, cyc)
    }

    #[test]
    fn a_second_claim_of_the_same_key_is_refused() {
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "notify", 0);
        assert!(s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap());
        assert!(
            !s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap(),
            "the unique key must refuse the second claim"
        );
        assert_eq!(
            s.actions_for(&id).unwrap().len(),
            1,
            "and must not write a second row"
        );
    }

    #[test]
    fn eight_threads_racing_one_key_produce_exactly_one_winner() {
        // The property a check-then-act in Rust would fail. This test is
        // the reason the claim is a PRIMARY KEY insert and not a SELECT
        // followed by an INSERT.
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec("k"), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        let k = action_key(&id, &cyc, 1, "notify", 0);
        let wins = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (p, k, id, cyc, wins) =
                    (d.path(), k.clone(), id.clone(), cyc.clone(), wins.clone());
                scope.spawn(move || {
                    let s = MonitorStore::open(p).unwrap();
                    if s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap() {
                        wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(wins.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn finishing_records_the_state_and_the_redacted_result() {
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "collect_logs", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap();
        s.finish_action(
            &k,
            ActionState::Done,
            "token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA ok",
        )
        .unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        assert_eq!(row.state, ActionState::Done);
        let result = row.result.as_deref().unwrap();
        assert!(
            !result.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "spec §安全與隱私: a secret must never reach history — got {result:?}"
        );
        assert!(
            result.contains("ok"),
            "the non-secret part must survive: {result:?}"
        );
    }

    #[test]
    fn redact_runs_before_the_length_cut_not_after() {
        // spec §安全與隱私, and rule 4 of this task: redact THEN truncate.
        // The token is placed straddling RESULT_MAX_CHARS (400): a
        // truncate-then-redact implementation hands the fixed-width secret
        // pattern only the first ~20 of its 40 characters, the pattern no
        // longer matches, and the fragment survives in stored history.
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "notify", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap();
        let filler = format!("{} ", "x".repeat(RESULT_MAX_CHARS - 21));
        let token = format!("ghp_{}", "A".repeat(36));
        let raw = format!("{filler}{token} tail");
        s.finish_action(&k, ActionState::Done, &raw).unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        let result = row.result.as_deref().unwrap();
        assert!(
            !result.contains("ghp_"),
            "a secret straddling the truncation cut must not survive as a fragment — got {result:?}"
        );
    }

    #[test]
    fn a_blocked_action_stays_pending_and_carries_its_approval_id() {
        // Blocked is NOT failed: a later tick must pick it up again.
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "rerun", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Write, t0())
            .unwrap();
        s.block_action(&k, "hitl-abc").unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        assert_eq!(row.state, ActionState::Blocked);
        assert_eq!(row.approval_id.as_deref(), Some("hitl-abc"));
        let pending: Vec<_> = s.pending_actions(t0(), 10, 10).unwrap();
        assert_eq!(
            pending.len(),
            1,
            "a blocked action must come back on a later tick"
        );
    }

    /// spec §錯誤處理: a gate failure is recorded against the action and
    /// retried, never propagated. The row must keep its state — a
    /// `finish_action`-style write would settle it, so a transient channel
    /// fault would permanently fail a remedy nobody ever attempted.
    #[test]
    fn recording_an_error_leaves_the_action_retryable() {
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "rerun", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Write, t0())
            .unwrap();
        s.record_action_error(
            &k,
            "approval gate failed: token=ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .unwrap();
        let row = &s.actions_for(&id).unwrap()[0];
        assert_eq!(
            row.state,
            ActionState::Claimed,
            "recording why must not settle the action"
        );
        let result = row.result.as_deref().unwrap();
        assert!(result.contains("approval gate failed"), "{result}");
        assert!(
            !result.contains("ghp_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            "a gate error reaches history like any other result and must be \
             redacted on the same chokepoint: {result}"
        );
        assert_eq!(
            s.pending_actions(t0(), 10, 10).unwrap().len(),
            1,
            "and the next tick must pick it back up"
        );
    }

    #[test]
    fn a_done_action_never_comes_back() {
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "notify", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Read, t0()).unwrap();
        s.finish_action(&k, ActionState::Done, "sent").unwrap();
        assert!(s.pending_actions(t0(), 10, 10).unwrap().is_empty());
    }

    /// `attempt` counts times the action was parked on something new, not
    /// times the drain re-gated it. Both halves matter: without the second,
    /// `attempt = attempt` (never counting) passes; without the first, the
    /// shipped `attempt + 1` passes and `show` reports thousands of
    /// "attempts" for a row nobody ever attempted.
    #[test]
    fn a_re_deferral_on_the_same_request_is_not_another_attempt() {
        let (_d, s, id, cyc) = fixture();
        let k = action_key(&id, &cyc, 1, "rerun", 0);
        s.claim_action(&k, &id, &cyc, RiskTier::Write, t0())
            .unwrap();
        s.block_action(&k, "h1").unwrap();
        for _ in 0..5 {
            s.block_action(&k, "h1").unwrap();
        }
        assert_eq!(
            s.actions_for(&id).unwrap()[0].attempt,
            1,
            "re-parking on the request already outstanding is the same wait, \
             not a new attempt"
        );
        // A genuinely different request IS a new attempt.
        s.block_action(&k, "h2").unwrap();
        assert_eq!(s.actions_for(&id).unwrap()[0].attempt, 2);
        assert_eq!(
            s.actions_for(&id).unwrap()[0].approval_id.as_deref(),
            Some("h2")
        );
    }
}
