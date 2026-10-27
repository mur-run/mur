//! `trash_into` (D6–D8): apply a remover's rules, move each target into the
//! agent's trash, enforce the size cap, and record every outcome in the ledger.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{Duration as ChronoDuration, Utc};
use mur_common::action::{
    ActionEvent, DeletionConfig, PermDeleteReason, RestoreMeta, TrashEntry, TrashStatus,
};
use mur_common::ledger::Ledger;
use uuid::Uuid;

use crate::detect::{Rule, TrashTarget};
use crate::layout::{entry_dir, entry_path, ledger_dir, staging_root, trash_root};
use crate::mover::{MoveOutcome, move_across, move_with_staging, remove_any};
use crate::safety::check_target;

const BYTES_PER_MB: u64 = 1024 * 1024;

/// What happened to one target.
#[derive(Debug)]
pub enum TrashOutcome {
    /// In the trash and recorded; restorable by `entry_id`.
    Trashed {
        target: TrashTarget,
        entry_id: Uuid,
        trash_path: PathBuf,
    },
    /// Cross-device copy is in the trash and recorded, but the original could
    /// not be removed. Reported as a failure: the delete did not happen.
    CopiedSourceKept {
        target: TrashTarget,
        entry_id: Uuid,
        error: String,
    },
    /// Over `trash_max_mb`: deleted for real (D7), recorded as an eviction.
    Evicted { target: TrashTarget, cap_mb: u64 },
    /// Missing under `-f`: nothing to do, nothing to say (as `rm -f`).
    MissingIgnored { target: TrashTarget },
    /// Not touched. `error` reads like the remover's own message.
    Failed { target: TrashTarget, error: String },
}

impl TrashOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::CopiedSourceKept { .. } | Self::Failed { .. })
    }
}

/// Move every target of a `Rewrite` plan into `agent_home`'s trash, in order.
///
/// Like the shell: within one `rm`, a bad operand does not stop the rest; but
/// once a target fails, later `&&` segments are skipped and get no outcome,
/// exactly as bash never runs them (review #2164 #2).
/// `task_id` goes into `TrashEntry::created_by_task_id` (use the turn id, or
/// `Uuid::nil()` when there is none).
pub fn trash_into(
    agent_home: &Path,
    targets: &[TrashTarget],
    config: &DeletionConfig,
    task_id: Uuid,
) -> Vec<TrashOutcome> {
    trash_into_with(
        agent_home,
        targets,
        config,
        task_id,
        &crate::publish::rename_noreplace,
    )
}

/// [`trash_into`] with an injectable `rename`, so tests can force EXDEV.
pub(crate) fn trash_into_with(
    agent_home: &Path,
    targets: &[TrashTarget],
    config: &DeletionConfig,
    task_id: Uuid,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> Vec<TrashOutcome> {
    let root = trash_root(agent_home);
    let cap = config.trash_max_mb.saturating_mul(BYTES_PER_MB);
    // Measured once, then kept current as this call adds to it.
    let mut used = tree_size(&root);
    let mut ledger = Ledger::<ActionEvent>::open(&ledger_dir(agent_home));

    let mut out = Vec::with_capacity(targets.len());
    let mut failed_segment: Option<usize> = None;
    for t in targets {
        if failed_segment.is_some_and(|s| t.segment > s) {
            break; // targets are in command order, so the rest are later too
        }
        let outcome = match ledger.as_mut() {
            Ok(l) => one(
                agent_home, &root, t, config, task_id, cap, &mut used, l, rename,
            ),
            Err(e) => failed(t, format!("cannot open trash ledger: {e}")),
        };
        if outcome.is_failure() {
            failed_segment.get_or_insert(t.segment);
        }
        out.push(outcome);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn one(
    agent_home: &Path,
    root: &Path,
    t: &TrashTarget,
    config: &DeletionConfig,
    task_id: Uuid,
    cap: u64,
    used: &mut u64,
    ledger: &mut Ledger<ActionEvent>,
    rename: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> TrashOutcome {
    let resolved = match check_target(&t.path, agent_home) {
        Ok(Ok(p)) => p,
        Ok(Err(refusal)) => return failed(t, refusal.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return match t.rule {
                Rule::Rm { force: true, .. } => TrashOutcome::MissingIgnored { target: t.clone() },
                _ => failed(t, "No such file or directory".into()),
            };
        }
        Err(e) => return failed(t, e.to_string()),
    };
    let meta = match fs::symlink_metadata(&resolved) {
        Ok(m) => m,
        Err(e) => return failed(t, e.to_string()),
    };
    if let Err(msg) = rule_allows(t.rule, &resolved, &meta) {
        return failed(t, msg.into());
    }

    let size = tree_size(&resolved);
    if used.saturating_add(size) > cap {
        return evict(t, &resolved, &meta, task_id, config.trash_max_mb, ledger);
    }

    let id = Uuid::new_v4();
    let Some(dst) = entry_path(root, id, &resolved) else {
        return failed(t, "path has no file name".into());
    };
    let now = Utc::now();
    let entry = TrashEntry {
        id,
        original_path: resolved.clone(),
        trash_path: Some(dst.clone()),
        file_size_bytes: size,
        created_by_task_id: task_id,
        created_at: now,
        execute_at: now,
        retention_until: Some(now + ChronoDuration::days(config.trash_retention_days.into())),
        status: TrashStatus::Retained,
        restore_metadata: restore_meta(&meta),
    };

    let kept = match move_with_staging(&resolved, &dst, &staging_root(root), rename) {
        Ok(MoveOutcome::Moved) => None,
        Ok(MoveOutcome::CopiedSourceKept(e)) => Some(e.to_string()),
        Err(e) => {
            let _ = fs::remove_dir(entry_dir(root, id));
            return failed(t, e.to_string());
        }
    };
    // Recorded only after the move, so the ledger never names a missing file.
    // If the record fails the object would be unrestorable: put it back.
    if let Err(e) = ledger.append(&ActionEvent::TrashCreated { entry }) {
        if kept.is_none() {
            let _ = move_across(&dst, &resolved);
        } else {
            let _ = remove_any(&dst);
        }
        let _ = fs::remove_dir(entry_dir(root, id));
        return failed(t, format!("not deleted: cannot record trash entry: {e}"));
    }
    *used = used.saturating_add(size);
    match kept {
        None => TrashOutcome::Trashed {
            target: t.clone(),
            entry_id: id,
            trash_path: dst,
        },
        Some(error) => TrashOutcome::CopiedSourceKept {
            target: t.clone(),
            entry_id: id,
            error,
        },
    }
}

/// D7: the target does not fit, so it is deleted for real. The ledger keeps a
/// `TrashCreated` (status `PermDeleted`, no `trash_path`) so `trash list`
/// shows what went, then the `CapacityEviction` record.
fn evict(
    t: &TrashTarget,
    resolved: &Path,
    meta: &fs::Metadata,
    task_id: Uuid,
    cap_mb: u64,
    ledger: &mut Ledger<ActionEvent>,
) -> TrashOutcome {
    if let Err(e) = remove_any(resolved) {
        return failed(t, e.to_string());
    }
    let now = Utc::now();
    let id = Uuid::new_v4();
    let entry = TrashEntry {
        id,
        original_path: resolved.to_path_buf(),
        trash_path: None,
        file_size_bytes: tree_size_meta(resolved, meta),
        created_by_task_id: task_id,
        created_at: now,
        execute_at: now,
        retention_until: None,
        status: TrashStatus::PermDeleted,
        restore_metadata: restore_meta(meta),
    };
    // The delete already happened; a ledger failure cannot undo it, so it is
    // not turned into a reported failure.
    let _ = ledger.append(&ActionEvent::TrashCreated { entry });
    let _ = ledger.append(&ActionEvent::TrashPermDeleted {
        entry_id: id,
        reason: PermDeleteReason::CapacityEviction,
    });
    TrashOutcome::Evicted {
        target: t.clone(),
        cap_mb,
    }
}

/// The remover's own preconditions, checked before anything moves.
fn rule_allows(rule: Rule, path: &Path, meta: &fs::Metadata) -> Result<(), &'static str> {
    let is_dir = meta.is_dir(); // symlink_metadata: a link to a dir is not a dir
    let empty = || fs::read_dir(path).map(|mut d| d.next().is_none());
    match rule {
        Rule::Rm {
            recursive: false,
            empty_dirs: false,
            ..
        } if is_dir => Err("is a directory"),
        Rule::Rm {
            recursive: false, ..
        } if is_dir => match empty() {
            Ok(true) => Ok(()),
            Ok(false) => Err("Directory not empty"),
            Err(_) => Err("Permission denied"),
        },
        Rule::Rm { .. } => Ok(()),
        Rule::Rmdir if !is_dir => Err("Not a directory"),
        Rule::Rmdir => match empty() {
            Ok(true) => Ok(()),
            Ok(false) => Err("Directory not empty"),
            Err(_) => Err("Permission denied"),
        },
        Rule::Unlink if is_dir => Err("is a directory"),
        Rule::Unlink => Ok(()),
    }
}

/// Bytes under `path`, never following links. Unreadable parts count as 0.
pub fn tree_size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) => tree_size_meta(path, &m),
        Err(_) => 0,
    }
}

fn tree_size_meta(path: &Path, meta: &fs::Metadata) -> u64 {
    if !meta.is_dir() {
        return meta.len();
    }
    fs::read_dir(path)
        .map(|rd| rd.flatten().map(|e| tree_size(&e.path())).sum())
        .unwrap_or(0)
}

#[cfg(unix)]
fn restore_meta(meta: &fs::Metadata) -> RestoreMeta {
    use std::os::unix::fs::MetadataExt;
    RestoreMeta {
        owner_uid: meta.uid(),
        owner_gid: meta.gid(),
        permissions: meta.mode(),
    }
}

#[cfg(not(unix))]
fn restore_meta(_meta: &fs::Metadata) -> RestoreMeta {
    RestoreMeta {
        owner_uid: 0,
        owner_gid: 0,
        permissions: 0,
    }
}

fn failed(t: &TrashTarget, error: String) -> TrashOutcome {
    TrashOutcome::Failed {
        target: t.clone(),
        error,
    }
}

/// D7: an `Unrewritable` / `Unparseable` delete is about to run as-is; keep
/// an audit line of it. The caller runs the command even if this fails.
pub fn record_unprotected(agent_home: &Path, command: &str, reason: &str) -> anyhow::Result<()> {
    let mut ledger = Ledger::<ActionEvent>::open(&ledger_dir(agent_home))?;
    ledger.append(&ActionEvent::UnprotectedDelete {
        command: command.to_owned(),
        reason: reason.to_owned(),
        at: Utc::now(),
    })
}
