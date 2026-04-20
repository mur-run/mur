//! Pre-filter pipeline orchestrator.
//! Runs normalize → dedup → filter → store.append with an audit entry per write.
//!
//! # Amendments applied
//! - **BP4 concurrency guard**: acquires an advisory flock on
//!   `<conversations_root>/.pull.lock` in `run()` so two concurrent
//!   `mur conversations pull` invocations can't interleave writes.
//! - **BP6 observability**: each stage is wrapped in a `tracing::info_span!`
//!   with structured attributes so `RUST_LOG=mur_core::conversations=info`
//!   surfaces per-stage timing.

use anyhow::{Result, anyhow};
use fs2::FileExt;
use mur_common::{Content, Message};
use std::fs::{File, OpenOptions};
use tracing::{info_span, warn};

use super::super::{
    audit::{Audit, AuditAction},
    paths, store,
};
use super::dedup::Dedup;
use super::filter::{Decision, decide};
use super::normalize::normalize;

#[derive(Debug, Default)]
pub struct Report {
    pub accepted: u64,
    pub rejected: u64,
    pub deduped: u64,
    pub errors: u64,
}

pub struct Pipeline {
    root_override: Option<String>,
    audit: Audit,
    dedup: Dedup,
}

enum Outcome {
    Accepted(u64),
    Rejected,
    Deduped,
}

impl Pipeline {
    pub fn new(root_override: Option<&str>) -> Result<Self> {
        Ok(Self {
            root_override: root_override.map(|s| s.to_string()),
            audit: Audit::open(root_override)?,
            dedup: Dedup::new(0.85),
        })
    }

    /// Run the pipeline over a batch of messages. Acquires a process-wide
    /// advisory lock (BP4) so two concurrent invocations can't interleave.
    pub fn run(&mut self, messages: Vec<Message>) -> Result<Report> {
        let _span = info_span!("conversations.pipeline.run", batch_size = messages.len()).entered();

        let _lock = self.acquire_pull_lock()?;

        let mut r = Report::default();
        for msg in messages {
            match self.process_one(msg) {
                Ok(Outcome::Accepted(bytes)) => {
                    r.accepted += 1;
                    let _ = self.audit.append(
                        AuditAction::Write {
                            target: "raw".into(),
                            bytes,
                        },
                        String::new(),
                    );
                }
                Ok(Outcome::Rejected) => r.rejected += 1,
                Ok(Outcome::Deduped) => r.deduped += 1,
                Err(e) => {
                    r.errors += 1;
                    warn!("pipeline error: {e:#}");
                    let _ = self.audit.append(
                        AuditAction::Error {
                            layer: "pipeline".into(),
                            reason: format!("{e:#}"),
                        },
                        String::new(),
                    );
                }
            }
        }
        Ok(r)
    }

    /// BP4 — exclusive advisory flock on `<conversations_root>/.pull.lock`.
    /// Returned guard is held for the entire `run()` and released on drop.
    fn acquire_pull_lock(&self) -> Result<PullLockGuard> {
        let dir = paths::conversations_root(self.root_override.as_deref());
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(".pull.lock");
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock_exclusive().map_err(|_| {
            anyhow!(
                "another `mur conversations pull` is running ({})",
                path.display()
            )
        })?;
        Ok(PullLockGuard { file: Some(file) })
    }

    fn process_one(&mut self, msg: Message) -> Result<Outcome> {
        // 1. Normalize (BP6 span)
        let msg = {
            let _s = info_span!("conversations.normalize").entered();
            normalize(msg, self.root_override.as_deref())?
        };
        // 2. Dedup (BP6 span; only on Text variant — ToolRef/ImageRef are content-addressed)
        let deduped = {
            let _s = info_span!("conversations.dedup").entered();
            if let Content::Text { value } = &msg.content {
                self.dedup.is_duplicate(value)
            } else {
                false
            }
        };
        if deduped {
            return Ok(Outcome::Deduped);
        }
        // 3. Filter (BP6 span)
        let verdict = {
            let _s = info_span!("conversations.filter").entered();
            decide(&msg)
        };
        if let Decision::Reject(reason) = verdict {
            tracing::debug!("rejected: {reason}");
            return Ok(Outcome::Rejected);
        }
        // 4. Store (BP6 span; serialize once for byte count)
        let bytes = {
            let _s = info_span!("conversations.store").entered();
            let serialized = serde_json::to_vec(&msg)?;
            let n = serialized.len() as u64;
            store::append(&msg, self.root_override.as_deref())?;
            n
        };
        Ok(Outcome::Accepted(bytes))
    }
}

/// RAII guard that releases the `.pull.lock` flock on drop.
struct PullLockGuard {
    file: Option<File>,
}

impl Drop for PullLockGuard {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = fs2::FileExt::unlock(&f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Content, Message, Role, Source};

    fn msg(role: Role, text: &str) -> Message {
        Message {
            v: 1,
            ts: chrono::Utc::now(),
            src: Source::ClaudeCode,
            conv: "c".into(),
            role,
            content: Content::Text { value: text.into() },
            meta: serde_json::Value::Null,
            refs: vec![],
        }
    }

    #[test]
    fn accepted_messages_are_written_and_audited() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let report = p
            .run(vec![msg(Role::User, "hi there how are you")])
            .unwrap();
        assert_eq!(report.accepted, 1);
        assert_eq!(report.rejected, 0);
        // Audit has a Write entry — P1 uses `"kind":"write"` tag, not `"action":"write"`.
        let entries =
            std::fs::read_to_string(crate::conversations::paths::audit_path(Some(root))).unwrap();
        assert!(
            entries.contains("\"kind\":\"write\""),
            "audit lacks write entry: {entries}"
        );
    }

    #[test]
    fn duplicate_messages_are_deduped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let r = p
            .run(vec![
                msg(Role::User, "the quick brown fox jumps over"),
                msg(Role::User, "the quick brown fox jumps over"),
            ])
            .unwrap();
        assert_eq!(r.accepted, 1);
        assert_eq!(r.deduped, 1);
    }

    #[test]
    fn rejected_messages_are_not_written() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut p = Pipeline::new(Some(root)).unwrap();
        let r = p.run(vec![msg(Role::User, "")]).unwrap();
        assert_eq!(r.accepted, 0);
        assert_eq!(r.rejected, 1);
    }

    #[test]
    fn concurrent_run_is_rejected() {
        // BP4: .pull.lock must exclude concurrent pipelines.
        use fs2::FileExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();

        // Prepare conversations dir so .pull.lock parent exists.
        std::fs::create_dir_all(crate::conversations::paths::conversations_root(Some(root)))
            .unwrap();
        let lock_path =
            crate::conversations::paths::conversations_root(Some(root)).join(".pull.lock");
        let held = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        held.try_lock_exclusive().unwrap();

        let mut p = Pipeline::new(Some(root)).unwrap();
        let err = p.run(vec![msg(Role::User, "x")]).unwrap_err();
        assert!(
            err.to_string().contains("another `mur conversations pull`"),
            "unexpected error: {err:#}"
        );

        FileExt::unlock(&held).unwrap();
    }
}
