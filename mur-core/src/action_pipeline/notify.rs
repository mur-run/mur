use mur_common::action::TaskOutcome;

/// Notification payload that the GUI bridge can render.
#[derive(Debug, Clone)]
pub struct ActionNotification {
    pub event_type: String,
    pub title: String,
    pub body: String,
    pub urgency: String,
    pub file_count: usize,
    pub item_id: Option<String>,
}

pub struct Aggregator;

impl Aggregator {
    /// Build completion notifications for a finished task.
    /// Same-batch results → 1 notification.
    pub fn build_completion_notifications(
        agent_name: &str,
        outcome: &TaskOutcome,
        _pending_item_count: usize,
    ) -> Vec<ActionNotification> {
        match outcome {
            TaskOutcome::Success { outputs } => {
                let count = outputs.len();
                vec![ActionNotification {
                    event_type: "task_completed".into(),
                    title: format!("{} completed", agent_name),
                    body: format!("{count} succeeded"),
                    urgency: "normal".into(),
                    file_count: count,
                    item_id: None,
                }]
            }
            TaskOutcome::PartialSuccess { succeeded, failed } => {
                vec![ActionNotification {
                    event_type: "task_completed".into(),
                    title: format!("{} completed", agent_name),
                    body: format!("{succeeded} succeeded, {failed} failed"),
                    urgency: "normal".into(),
                    file_count: (succeeded + failed) as usize,
                    item_id: None,
                }]
            }
            TaskOutcome::Failed { error } => {
                vec![ActionNotification {
                    event_type: "task_failed".into(),
                    title: format!("{} failed", agent_name),
                    body: error.clone(),
                    urgency: "high".into(),
                    file_count: 0,
                    item_id: None,
                }]
            }
        }
    }

    /// Build a deletion notification (independent from completion,
    /// always high urgency).
    pub fn build_deletion_notification(
        agent_name: &str,
        file_count: usize,
        cancel_window_minutes: u32,
    ) -> ActionNotification {
        ActionNotification {
            event_type: "deletion_pending".into(),
            title: format!("{agent_name} wants to delete {file_count} files"),
            body: format!("Moving to Trash in {cancel_window_minutes} min · recoverable"),
            urgency: "high".into(),
            file_count,
            item_id: None,
        }
    }

    /// Badge number = pending items + running tasks.
    /// Completed and failed are excluded.
    pub fn badge_count(
        pending_items: usize,
        running_tasks: usize,
        _completed_count: usize,
        _failed_count: usize,
    ) -> usize {
        pending_items + running_tasks
    }

    /// Build the structured notification metadata (Courier dual-audience format).
    pub fn build_ingest_notification(
        item_id: &str,
        file_count: usize,
        _mime_types: &[String],
        file_names: &[String],
    ) -> ActionNotification {
        let body = if file_names.len() <= 3 {
            file_names.join(", ")
        } else {
            format!(
                "{} and {} more",
                file_names[..3].join(", "),
                file_names.len() - 3
            )
        };

        ActionNotification {
            event_type: "pending_item_ingested".into(),
            title: format!("{file_count} files received"),
            body,
            urgency: "low".into(),
            file_count,
            item_id: Some(item_id.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::action::{ActionOutput, OutputKind, TaskOutcome};

    #[test]
    fn aggregate_same_batch_single_notification() {
        let outputs = vec![
            ActionOutput {
                kind: OutputKind::File,
                file_path: Some("/tmp/a.txt".into()),
                chat_content: None,
            },
            ActionOutput {
                kind: OutputKind::File,
                file_path: Some("/tmp/b.txt".into()),
                chat_content: None,
            },
        ];
        let notifications = Aggregator::build_completion_notifications(
            "TestAgent",
            &TaskOutcome::Success { outputs },
            1, // pending_item_count
        );
        // One aggregated notification
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].body.contains("2 succeeded"));
    }

    #[test]
    fn partial_success_reports_counts() {
        let outcome = TaskOutcome::PartialSuccess {
            succeeded: 3,
            failed: 2,
        };
        let notifications = Aggregator::build_completion_notifications("Agent", &outcome, 0);
        assert!(notifications[0].body.contains("3 succeeded"));
        assert!(notifications[0].body.contains("2 failed"));
    }

    #[test]
    fn badge_count_is_pending_plus_running() {
        let badge = Aggregator::badge_count(3, 2, 5, 1);
        // pending(3) + running(2) = 5; completed(5) + failed(1) excluded
        assert_eq!(badge, 5);
    }

    #[test]
    fn deletion_notification_is_independent() {
        let n = Aggregator::build_deletion_notification(
            "Agent", 3, 10, // cancel_window_minutes
        );
        assert!(n.title.contains("3 files"));
        assert!(n.body.contains("10 min"));
        assert_eq!(n.urgency, "high");
    }
}
