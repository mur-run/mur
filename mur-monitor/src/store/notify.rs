//! The delivery queue over `monitor_notifications` (created empty by the
//! read-only slice, never written to before this task). One row per
//! (event, channel).
//!
//! **The key is the EVENT ROW ID, not `(monitor, cycle, kind)`** as
//! §通知策略 literally says. That wording predates the discovery that
//! `cycle_id` never rotates for a monitor: keying on it would silence every
//! episode after the first, which is the exact defect the read-only slice's
//! final review found and fixed one layer down, in the events table. Every
//! notifiable kind is written under a transition guard, so one event row is
//! already one real transition — a monitor that stalls, recovers, and
//! stalls again gets two `stalled` rows and therefore two notifications.
//!
//! `monitor_notifications` has no `attempts` column and this plan does not
//! alter the schema (the shipped `migrate()` is `CREATE TABLE IF NOT
//! EXISTS`, and an `ALTER` would need a `user_version` step this plan does
//! not want to introduce). Attempts are therefore encoded in the
//! `delivery_state` column itself as `"<state>:<attempts>"` (e.g.
//! `"pending:3"`) rather than kept in a side table — one table stays the
//! whole footprint of this feature, and `DeliveryState::parse` already has
//! to tolerate unknown suffixes for forward-compatibility, so accepting the
//! attempt count there costs nothing extra. `DeliveryState` itself stays the
//! fieldless three-variant enum callers expect; only the free functions in
//! this module know about the `:N` suffix.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, params};

use super::{EventRow, MonitorRow, MonitorStore, parse_ts, ts};
use crate::backoff::unknown_delay;
use crate::notify::NOTIFIABLE;

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

    /// Accepts both the bare state (`"pending"`, as `as_str` produces) and
    /// this module's attempt-count encoding (`"pending:3"`) — see the
    /// module doc for why the count lives inside this string rather than a
    /// schema column. Round-trips: `parse(state.as_str()) == Some(state)`
    /// for every variant.
    pub fn parse(s: &str) -> Option<Self> {
        let base = s.split(':').next().unwrap_or(s);
        match base {
            "pending" => Some(DeliveryState::Pending),
            "delivered" => Some(DeliveryState::Delivered),
            "failed" => Some(DeliveryState::Failed),
            _ => None,
        }
    }
}

/// Encode a state plus its attempt count into the single TEXT column
/// `monitor_notifications.delivery_state`. The only place that knows the
/// `":N"` suffix — everything else goes through `DeliveryState`/`parse`/
/// `decode_attempts`.
fn encode_state(state: DeliveryState, attempts: u32) -> String {
    format!("{}:{attempts}", state.as_str())
}

fn decode_attempts(raw: &str) -> u32 {
    raw.split_once(':')
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0)
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
    /// 0 when no row exists yet for this (event, channel) — nothing has
    /// ever been attempted.
    fn delivery_attempts(&self, event_id: i64, channel: &str) -> Result<u32> {
        let raw: Option<String> = self
            .conn()
            .query_row(
                "SELECT delivery_state FROM monitor_notifications WHERE event_key = ?1",
                [key(event_id, channel)],
                |r| r.get(0),
            )
            .optional()?;
        Ok(raw.as_deref().map(decode_attempts).unwrap_or(0))
    }

    /// Notifiable events with no delivered/failed row for this channel, and
    /// whose retry time (if any) has arrived. Oldest first — someone reading
    /// a backlog wants it in the order it happened.
    ///
    /// Every placeholder is numbered explicitly, including the kind list
    /// generated below (`?4, ?5, …`) — never a bare `?`, which SQLite binds
    /// by argument-vector position rather than by the number written here.
    /// Mixing the two styles happens to work only as long as the vector is
    /// built in exactly the order the bare `?`s expect; reorder it and a
    /// channel name silently lands in a kind slot, matching nothing and
    /// reporting "no pending notifications" instead of failing loudly.
    pub fn pending_notifications(
        &self,
        channel: &str,
        now: DateTime<Utc>,
        max: usize,
    ) -> Result<Vec<Pending>> {
        let kind_placeholders: Vec<String> = (0..NOTIFIABLE.len())
            .map(|i| format!("?{}", i + 4))
            .collect();
        let sql = format!(
            "SELECT e.id, e.monitor_id, e.cycle_id, e.kind, e.payload, e.created_at, n.delivery_state \
             FROM monitor_events e \
             LEFT JOIN monitor_notifications n ON n.event_key = (e.id || ':' || ?1) \
             WHERE e.kind IN ({kinds}) \
               AND (n.delivery_state IS NULL \
                    OR (n.delivery_state LIKE 'pending%' AND n.updated_at <= ?2)) \
             ORDER BY e.id ASC LIMIT ?3",
            kinds = kind_placeholders.join(", "),
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![
            Box::new(channel.to_string()),
            Box::new(ts(now)),
            Box::new(max as i64),
        ];
        for k in NOTIFIABLE {
            args.push(Box::new(k.to_string()));
        }
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|b| b.as_ref())),
            |r| {
                let payload: String = r.get(4)?;
                let raw_state: Option<String> = r.get(6)?;
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    EventRow {
                        cycle_id: r.get(2)?,
                        kind: r.get(3)?,
                        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                        created_at: parse_ts(&r.get::<_, String>(5)?),
                    },
                    raw_state,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (event_id, monitor_id, event, raw_state) = row?;
            // Monitors are never deleted today; this is defensive rather
            // than reachable, but a vanished monitor has nothing sane to
            // notify about.
            let Some(row) = self.get(&monitor_id)? else {
                continue;
            };
            let attempts = raw_state.as_deref().map(decode_attempts).unwrap_or(0);
            out.push(Pending {
                event_id,
                row,
                event,
                attempts,
            });
        }
        Ok(out)
    }

    pub fn mark_delivered(&self, event_id: i64, channel: &str, now: DateTime<Utc>) -> Result<()> {
        let attempts = self.delivery_attempts(event_id, channel)?;
        self.conn().execute(
            "INSERT INTO monitor_notifications (event_key, monitor_id, channel, delivery_state, updated_at) \
             VALUES (?1, (SELECT monitor_id FROM monitor_events WHERE id = ?2), ?3, ?4, ?5) \
             ON CONFLICT(event_key) DO UPDATE SET delivery_state = ?4, updated_at = ?5",
            params![
                key(event_id, channel),
                event_id,
                channel,
                encode_state(DeliveryState::Delivered, attempts),
                ts(now),
            ],
        )?;
        Ok(())
    }

    /// Records a failed attempt and returns the resulting state: `Pending`
    /// while attempts remain, with the next attempt pushed out by the
    /// unknown-backoff curve (this is our problem to notice quickly, same
    /// curve `unknown_delay` gives a source that will not answer); `Failed`
    /// once `DELIVERY_MAX_ATTEMPTS` is hit. A parked `Failed` row is excluded
    /// by `pending_notifications` unconditionally — `updated_at` is not
    /// consulted for it — so it is never retried, however far `now` moves.
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
        // `updated_at` doubles as "not before" for a still-pending row.
        let not_before = now
            + chrono::Duration::from_std(unknown_delay(attempts - 1))
                .unwrap_or_else(|_| chrono::Duration::seconds(60));
        self.conn().execute(
            "INSERT INTO monitor_notifications (event_key, monitor_id, channel, delivery_state, updated_at) \
             VALUES (?1, (SELECT monitor_id FROM monitor_events WHERE id = ?2), ?3, ?4, ?5) \
             ON CONFLICT(event_key) DO UPDATE SET delivery_state = ?4, updated_at = ?5",
            params![
                key(event_id, channel),
                event_id,
                channel,
                encode_state(state, attempts),
                ts(not_before),
            ],
        )?;
        Ok(state)
    }

    /// Every delivery row recorded for a monitor's events, across channels.
    pub fn delivery_states(
        &self,
        monitor_id: &str,
    ) -> Result<Vec<(i64, String, DeliveryState, u32)>> {
        let mut stmt = self.conn().prepare(
            "SELECT e.id, n.channel, n.delivery_state FROM monitor_notifications n \
             JOIN monitor_events e ON (e.id || ':' || n.channel) = n.event_key \
             WHERE n.monitor_id = ?1 ORDER BY e.id ASC, n.channel ASC",
        )?;
        let rows = stmt.query_map([monitor_id], |r| {
            let raw: String = r.get(2)?;
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, raw))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, channel, raw) = row?;
            let state = DeliveryState::parse(&raw).unwrap_or(DeliveryState::Pending);
            let attempts = decode_attempts(&raw);
            out.push((id, channel, state, attempts));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{spec, t0};

    fn store_with_events(kinds: &[&'static str]) -> (tempfile::TempDir, MonitorStore, String) {
        let d = tempfile::tempdir().unwrap();
        let s = MonitorStore::open(d.path()).unwrap();
        let id = s.create(&spec("k"), t0(), None).unwrap().id;
        let cyc = s.get(&id).unwrap().unwrap().cycle_id;
        for k in kinds {
            s.append_event(&id, &cyc, k, serde_json::json!({}), false, t0())
                .unwrap();
        }
        (d, s, id)
    }

    #[test]
    fn only_notifiable_kinds_are_queued() {
        // `created` is written by `create` itself; the rest are ours.
        let (_d, s, _id) = store_with_events(&["observed", "stalled", "retried", "terminal"]);
        let p = s.pending_notifications("log", t0(), 10).unwrap();
        let kinds: Vec<_> = p.iter().map(|x| x.event.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["stalled", "terminal"],
            "bookkeeping must not queue"
        );
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
        s.append_event(
            &id,
            &cyc,
            "stalled_recovered",
            serde_json::json!({}),
            false,
            t0(),
        )
        .unwrap();
        s.append_event(&id, &cyc, "stalled", serde_json::json!({}), false, t0())
            .unwrap();

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
                assert_eq!(
                    state,
                    DeliveryState::Pending,
                    "attempt {i} must stay retryable"
                );
                // not due yet: the backoff pushes the next attempt out
                assert!(s.pending_notifications("log", t0(), 10).unwrap().is_empty());
            }
        }
        assert_eq!(state, DeliveryState::Failed);
        assert!(
            s.pending_notifications("log", t0() + chrono::Duration::days(1), 10)
                .unwrap()
                .is_empty(),
            "a parked failure must not be retried forever"
        );
    }

    #[test]
    fn max_bounds_a_drain() {
        let (_d, s, _id) = store_with_events(&["stalled", "soft_deadline", "terminal"]);
        assert_eq!(s.pending_notifications("log", t0(), 2).unwrap().len(), 2);
    }
}
