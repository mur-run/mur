//! `GET /api/v1/schedules` — the local, account-free schedule view.
//!
//! Served from [`crate::schedule_status::schedule_status`], the aggregator that
//! already folds agent, workflow and fleet schedules into one list for the
//! Panel. This adds a consumer to an existing derivation rather than a second
//! derivation of the same thing.
//!
//! **Read-only, deliberately.** Writing a schedule here would mean editing an
//! agent profile, a fleet file or an OS-level timetable — three different
//! owners, all of which the CLI already covers (`mur agent schedule`,
//! `mur fleet`). The Dashboard says editing lives there instead of offering a
//! control that would 404.
//!
//! See `docs/superpowers/specs/2026-08-30-hub-dashboard-surface-split-design.md`, D3a.

use std::sync::Arc;

use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;

use super::{AppError, AppState, wrap};
use crate::schedule_status::{ScheduleItem, schedule_status};

/// One schedule, flattened for the Dashboard.
///
/// `description` and `next_fires` are filled here and never recomputed in the
/// browser (surface-split design, D4). `next_note` carries the reason whenever
/// `next_fires` is empty — a blank "Next" reads as "will not run again", which
/// for an interval-triggered fleet is simply false.
#[derive(Serialize)]
pub(super) struct ScheduleRecord {
    id: String,
    /// Agent, workflow or fleet the schedule belongs to.
    workflow_name: String,
    /// The trigger as written in the profile / fleet file / timetable.
    cron_expr: String,
    enabled: bool,
    description: String,
    next_fires: Vec<String>,
    next_note: Option<String>,
    /// `agent-cron` | `agent-idle` | `workflow` | `fleet` — the scope a row
    /// belongs to, so an agent-scoped view can tell its own rows from globals.
    kind: &'static str,
}

impl From<ScheduleItem> for ScheduleRecord {
    fn from(item: ScheduleItem) -> Self {
        let (kind, owner, expr, enabled, description, next_fires, next_note) = match item {
            ScheduleItem::AgentCron {
                owner,
                expr,
                status,
                description,
                next_fires,
                next_note,
                ..
            } => (
                "agent-cron",
                owner,
                expr,
                status == "enabled",
                description,
                next_fires,
                next_note,
            ),
            ScheduleItem::AgentIdle {
                owner,
                after_secs,
                status,
                description,
                next_note,
                ..
            } => (
                "agent-idle",
                owner,
                format!("idle:{after_secs}s"),
                status == "enabled",
                description,
                Vec::new(),
                next_note,
            ),
            ScheduleItem::Workflow {
                owner,
                expr,
                status,
                description,
                next_fires,
                next_note,
            } => (
                "workflow",
                owner,
                expr.unwrap_or_else(|| "manual".into()),
                status == "enabled",
                description,
                next_fires,
                next_note,
            ),
            ScheduleItem::Fleet {
                owner,
                trigger,
                status,
                description,
                next_fires,
                next_note,
                ..
            } => (
                "fleet",
                owner,
                trigger,
                status == "enabled",
                description,
                next_fires,
                next_note,
            ),
        };
        Self {
            id: format!("{kind}:{owner}:{expr}"),
            workflow_name: owner,
            cron_expr: expr,
            enabled,
            description,
            next_fires,
            next_note,
            kind,
        }
    }
}

pub(super) async fn list_schedules(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, AppError> {
    let status = schedule_status(&state.mur_home(), None);
    let records: Vec<ScheduleRecord> = status.schedules.into_iter().map(Into::into).collect();
    let count = records.len();
    Ok(wrap(records, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant the Dashboard depends on: an empty `next_fires` never
    /// arrives without a reason attached.
    #[test]
    fn empty_next_fires_always_carries_a_note() {
        let item = ScheduleItem::Fleet {
            owner: "nightly".into(),
            trigger: "interval:30m".into(),
            next_fires: Vec::new(),
            status: "enabled".into(),
            budget_usd: 0.0,
            autorun_env: false,
            description: "every 30m".into(),
            next_note: Some("not tracked — an interval fires relative to its last run".into()),
        };
        let rec = ScheduleRecord::from(item);
        assert!(rec.next_fires.is_empty());
        assert!(rec.next_note.is_some(), "a blank Next must say why");
        assert_eq!(rec.kind, "fleet");
        assert_eq!(rec.id, "fleet:nightly:interval:30m");
    }

    #[test]
    fn a_stopped_fleet_is_not_enabled() {
        let item = ScheduleItem::Fleet {
            owner: "nightly".into(),
            trigger: "cron:0 3 * * *".into(),
            next_fires: vec!["2026-09-01T03:00:00+00:00".into()],
            status: "stopped".into(),
            budget_usd: 0.0,
            autorun_env: false,
            description: "daily at 03:00".into(),
            next_note: None,
        };
        assert!(!ScheduleRecord::from(item).enabled);
    }
}
