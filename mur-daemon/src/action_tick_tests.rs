//! Deferred trash moves use the D3 layout and the shared mover (spec §5).

use super::*;
use chrono::Duration as ChronoDuration;
use mur_common::action::{ActionPipelineConfig, RestoreMeta, TrashEntry};
use std::fs;

fn pending(original: std::path::PathBuf) -> TrashEntry {
    let now = Utc::now();
    TrashEntry {
        id: Uuid::new_v4(),
        original_path: original,
        trash_path: None,
        file_size_bytes: 1,
        created_by_task_id: Uuid::nil(),
        created_at: now,
        execute_at: now - ChronoDuration::minutes(1),
        retention_until: None,
        status: TrashStatus::PendingDelete,
        restore_metadata: RestoreMeta {
            owner_uid: 0,
            owner_gid: 0,
            permissions: 0,
        },
    }
}

fn setup() -> (tempfile::TempDir, Pipeline) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("agent");
    fs::create_dir_all(home.join("work")).unwrap();
    (tmp, Pipeline::new(home, ActionPipelineConfig::default()))
}

fn created(p: &Pipeline) -> Vec<TrashEntry> {
    ActionLedger::replay_days(&p.ledger_dir(), 1)
        .into_iter()
        .filter_map(|e| match e {
            ActionEvent::TrashCreated { entry } => Some(entry),
            _ => None,
        })
        .collect()
}

#[test]
fn same_name_twice_lands_in_distinct_entry_dirs() {
    let (_t, p) = setup();
    let file = p.agent_home.join("work/a.txt");
    let mut ids = Vec::new();
    for body in ["one", "two"] {
        fs::write(&file, body).unwrap();
        let e = pending(file.clone());
        ids.push(e.id);
        execute_move_to_trash(&p, &e).unwrap();
        assert!(!file.exists());
    }
    let recorded = created(&p);
    assert_eq!(recorded.len(), 2);
    for (entry, (id, body)) in recorded.iter().zip(ids.iter().zip(["one", "two"])) {
        let want = p.trash_dir().join(id.to_string()).join("a.txt");
        assert_eq!(entry.trash_path.as_deref(), Some(want.as_path()));
        assert_eq!(fs::read_to_string(&want).unwrap(), body);
        assert_eq!(entry.status, TrashStatus::Retained);
    }
}

#[test]
fn directory_is_moved_whole() {
    let (_t, p) = setup();
    let dir = p.agent_home.join("work/build");
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/x"), "x").unwrap();
    let e = pending(dir.clone());
    execute_move_to_trash(&p, &e).unwrap();
    assert!(!dir.exists());
    let moved = p.trash_dir().join(e.id.to_string()).join("build/sub/x");
    assert!(moved.exists());
}

#[test]
fn refused_target_is_left_and_not_recorded() {
    let (_t, p) = setup();
    let inside = p.trash_dir().join("old/a.txt");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, "x").unwrap();
    execute_move_to_trash(&p, &pending(inside.clone())).unwrap();
    assert!(inside.exists());
    assert!(created(&p).is_empty());
}

#[test]
fn only_unsettled_past_window_entries_are_due() {
    let a = pending("/x/a".into()); // already moved
    let b = pending("/x/b".into()); // cancelled
    let c = pending("/x/c".into()); // due
    let mut d = pending("/x/d".into()); // window still open
    d.execute_at = Utc::now() + ChronoDuration::minutes(5);
    let events = vec![
        ActionEvent::DeletionPending { entry: a.clone() },
        ActionEvent::TrashCreated { entry: a },
        ActionEvent::DeletionPending { entry: b.clone() },
        ActionEvent::DeletionCancelled { entry_id: b.id },
        ActionEvent::DeletionPending { entry: c.clone() },
        ActionEvent::DeletionPending { entry: d },
    ];
    let due: Vec<_> = due_pending(&events, Utc::now())
        .iter()
        .map(|e| e.id)
        .collect();
    assert_eq!(due, vec![c.id]);
}

#[test]
fn settled_ids_cover_created_and_cancelled() {
    let a = pending("/x/a".into());
    let b = pending("/x/b".into());
    let c = pending("/x/c".into());
    let events = vec![
        ActionEvent::DeletionPending { entry: a.clone() },
        ActionEvent::TrashCreated { entry: a.clone() },
        ActionEvent::DeletionPending { entry: b.clone() },
        ActionEvent::DeletionCancelled { entry_id: b.id },
        ActionEvent::DeletionPending { entry: c.clone() },
    ];
    let s = settled_ids(&events);
    assert!(s.contains(&a.id) && s.contains(&b.id));
    assert!(!s.contains(&c.id));
}
