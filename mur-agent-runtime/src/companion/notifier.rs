//! Notifier trait + CompanionMessage + NotifyOutcome + StdoutNotifier.
//! Spec §4.9.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mur_common::companion::Situation;

use crate::companion::picker::TemplateId;

// Re-export so callers can use `notifier::TemplateId` if desired.
pub use crate::companion::picker::TemplateId as NotifierTemplateId;

// ─── Types ───────────────────────────────────────────────────────────────────

/// A companion message ready for delivery.
#[derive(Debug, Clone)]
pub struct CompanionMessage {
    pub id: String,
    pub situation: Situation,
    pub template_id: TemplateId,
    pub locale: String,
    pub body: String,
    pub generated_at: DateTime<Utc>,
}

/// Result of a single `Notifier::send` call.
pub enum NotifyOutcome {
    Delivered,
    Skipped { reason: String },
    Failed(anyhow::Error),
}

// ─── Trait ───────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Notifier: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, msg: &CompanionMessage) -> Result<NotifyOutcome>;
}

// ─── StdoutNotifier ──────────────────────────────────────────────────────────

/// Writes each message to `<inbox_dir>/<id>.md` and optionally prints a
/// single-line banner to stderr when running in a TTY.
pub struct StdoutNotifier {
    pub inbox_dir: PathBuf,
}

impl StdoutNotifier {
    pub fn new(inbox_dir: PathBuf) -> Self {
        Self { inbox_dir }
    }
}

#[async_trait]
impl Notifier for StdoutNotifier {
    fn name(&self) -> &'static str {
        "stdout"
    }

    async fn send(&self, msg: &CompanionMessage) -> Result<NotifyOutcome> {
        match crate::companion::inbox::write_inbox_md(&self.inbox_dir, msg) {
            Ok(_) => {
                // Print banner to stderr only when running in an interactive TTY
                // and `MUR_COMPANION_BANNER` is not "off".
                let banner_off = std::env::var("MUR_COMPANION_BANNER")
                    .map(|v| v == "off")
                    .unwrap_or(false);
                if !banner_off
                    && std::io::IsTerminal::is_terminal(&std::io::stderr())
                {
                    let id_short = msg.id.chars().take(8).collect::<String>();
                    eprintln!(
                        "[mur-companion] {:?} → {} (id={})",
                        msg.situation, msg.locale, id_short
                    );
                }
                Ok(NotifyOutcome::Delivered)
            }
            Err(e) => {
                // Check if the underlying cause is AlreadyExists.
                if let Some(io_err) = e.downcast_ref::<std::io::Error>()
                    && io_err.kind() == std::io::ErrorKind::AlreadyExists
                {
                    return Ok(NotifyOutcome::Skipped {
                        reason: "already_delivered".to_string(),
                    });
                }
                Ok(NotifyOutcome::Failed(e))
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use mur_common::companion::Situation;
    use tempfile::TempDir;

    fn sample_msg() -> CompanionMessage {
        CompanionMessage {
            id: "test_notifier_id_01".to_string(),
            situation: Situation::GentleCheckIn,
            template_id: "check_in_zh_001".to_string(),
            locale: "zh-TW".to_string(),
            body: "最近怎樣？".to_string(),
            generated_at: Utc.with_ymd_and_hms(2026, 4, 29, 10, 0, 0).unwrap(),
        }
    }

    #[tokio::test]
    async fn happy_path_returns_delivered_and_file_exists() {
        let dir = TempDir::new().unwrap();
        let notifier = StdoutNotifier::new(dir.path().to_path_buf());
        let msg = sample_msg();

        let outcome = notifier.send(&msg).await.expect("send must not error");
        assert!(
            matches!(outcome, NotifyOutcome::Delivered),
            "expected Delivered"
        );

        let path = dir.path().join(format!("{}.md", msg.id));
        assert!(path.exists(), "inbox file must exist after send");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("situation: gentle_check_in"));
        assert!(content.contains("locale: zh-TW"));
        assert!(content.contains("最近怎樣？"));
        assert!(content.ends_with(">>> response: <unset>"));
    }

    #[tokio::test]
    async fn duplicate_send_returns_skipped_original_unchanged() {
        let dir = TempDir::new().unwrap();
        let notifier = StdoutNotifier::new(dir.path().to_path_buf());
        let msg = sample_msg();

        let first = notifier.send(&msg).await.expect("first send");
        assert!(matches!(first, NotifyOutcome::Delivered));

        let path = dir.path().join(format!("{}.md", msg.id));
        let original_content = std::fs::read_to_string(&path).unwrap();
        let original_modified = path.metadata().unwrap().modified().unwrap();

        let second = notifier.send(&msg).await.expect("second send");
        match second {
            NotifyOutcome::Skipped { reason } => {
                assert_eq!(reason, "already_delivered");
            }
            other => panic!(
                "expected Skipped{{already_delivered}}, got {:?}",
                match other {
                    NotifyOutcome::Delivered => "Delivered",
                    NotifyOutcome::Failed(_) => "Failed",
                    NotifyOutcome::Skipped { .. } => "Skipped",
                }
            ),
        }

        // Original file must be unchanged.
        let after_content = std::fs::read_to_string(&path).unwrap();
        let after_modified = path.metadata().unwrap().modified().unwrap();
        assert_eq!(original_content, after_content, "file content must not change");
        assert_eq!(
            original_modified, after_modified,
            "file mtime must not change"
        );
    }
}
