use super::*;

/// Pure decision for the orphan sweep, isolated for testing.
///
/// Prune iff we have both paths recorded AND the repo root still exists (drive
/// mounted / repo present) AND the worktree path is gone. When the repo root is
/// also missing the situation is ambiguous (unmounted external drive vs. deleted
/// repo) so we KEEP — never delete an index just because its volume is offline.
pub(super) fn orphan_should_prune(
    repo_root: &str,
    project_path: &str,
    repo_exists: bool,
    worktree_exists: bool,
) -> bool {
    !repo_root.is_empty() && !project_path.is_empty() && repo_exists && !worktree_exists
}

/// Garbage-collect codebase indexes whose linked worktree was removed, reclaiming
/// their disk. Safe on external drives (see [`orphan_should_prune`]). Skips any
/// index whose lock is held by a live worker.
pub fn prune_orphan_indexes() {
    let indexes_dir = crate::paths::mur_root(None)
        .join("indexes")
        .join("codebase");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".lance") || !p.is_dir() {
            continue;
        }
        let key = name.trim_end_matches(".lance");
        let meta_path = indexes_dir.join(format!("{key}.meta.json"));
        let Ok(data) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<IndexMetadata>(&data) else {
            continue;
        };
        let repo_exists = Path::new(&meta.repo_root).exists();
        let wt_exists = Path::new(&meta.project_path).exists();
        if !orphan_should_prune(&meta.repo_root, &meta.project_path, repo_exists, wt_exists) {
            continue;
        }
        // Don't yank an index out from under a live worker.
        let lock_path = indexes_dir.join(format!("{key}.lock"));
        if let Ok(ld) = std::fs::read_to_string(&lock_path)
            && let Ok(lock) = serde_json::from_str::<IndexLock>(&ld)
            && mur_common::lock_file::pid_alive(lock.pid)
        {
            continue;
        }
        let idx = CodebaseIndex::new(key, Path::new(&meta.project_path));
        if idx.delete_index().is_ok() {
            tracing::info!(
                index = key,
                "pruned orphan codebase index (worktree removed)"
            );
        }
    }
}

pub fn discover_all_indexes() -> Vec<DiscoveredIndex> {
    // GC removed-worktree indexes before listing so callers see only live ones.
    prune_orphan_indexes();
    let indexes_dir = crate::paths::mur_root(None)
        .join("indexes")
        .join("codebase");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return Vec::new();
    };

    let mut results = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with(".lance") || !path.is_dir() {
            continue;
        }
        let project_name = name.trim_end_matches(".lance");
        let meta_path = indexes_dir.join(format!("{project_name}.meta.json"));
        let (project_path, last_indexed, file_count) =
            if let Ok(data) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str::<IndexMetadata>(&data) {
                    let pp = if meta.project_path.is_empty() {
                        None
                    } else {
                        Some(meta.project_path)
                    };
                    (pp, Some(meta.last_indexed), meta.files.len())
                } else {
                    (None, None, 0)
                }
            } else {
                (None, None, 0)
            };

        results.push(DiscoveredIndex {
            name: project_name.to_string(),
            project_path,
            last_indexed,
            file_count,
        });
    }
    results.sort_by_key(|a| a.name.to_lowercase());
    results
}
