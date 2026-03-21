//! Schedule — unified schedule definitions shared between mur CLI and Commander.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How to handle missed schedule executions.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    /// Skip missed executions
    #[default]
    Skip,
    /// Run only the latest missed execution
    RunLatest,
    /// Run all missed executions
    RunAll,
}

/// Who is ticking this schedule.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleExecutor {
    /// System cron/launchd (mur CLI solo mode)
    #[default]
    SystemCron,
    /// Commander daemon tick loop
    Commander,
    /// Server-side tick
    Server,
}

/// Notification target for schedule results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleNotify {
    /// Notification type: slack_dm, slack_channel, terminal, webhook, none
    #[serde(default, rename = "type")]
    pub notify_type: String,
    /// Target channel/user ID
    #[serde(default)]
    pub target: String,
}

/// Capability required to execute a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Shell,
    Docker,
    Browser,
    Ai,
    Http,
    Ssh,
}

/// A schedule definition — the canonical shared type.
///
/// Used in `~/.mur/schedules.yaml` and synced to server.
/// Commander adds runtime state (last_run, next_run, etc.) separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    /// Unique schedule ID
    pub id: String,
    /// Workflow name to execute
    pub workflow: String,
    /// Cron expression (5-7 fields)
    pub cron: String,
    /// Timezone (e.g. "Asia/Taipei", "UTC")
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Whether this schedule is active
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Owner user ID (empty for solo mode)
    #[serde(default)]
    pub user_id: String,
    /// Variables to pass to the workflow
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub variables: HashMap<String, String>,
    /// Notification config
    #[serde(default)]
    pub notify: ScheduleNotify,
    /// What to do when executions are missed
    #[serde(default)]
    pub on_missed: MissedPolicy,
    /// Who is currently ticking this schedule
    #[serde(default)]
    pub executor: ScheduleExecutor,
}

fn default_timezone() -> String { "UTC".into() }
fn default_enabled() -> bool { true }

/// Container for schedules.yaml file format.
#[derive(Debug, Serialize, Deserialize)]
pub struct SchedulesFile {
    #[serde(default)]
    pub schedules: Vec<Schedule>,
}
