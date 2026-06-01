# `mur project remove` — Remove an indexed project

**Date:** 2026-06-01
**Status:** Draft
**Target:** mur binary (`mur-core` crate)

## Summary

Add `mur project remove [path]` to remove a project from the codebase index.
Deletes the LanceDB vector database, metadata, lock, and progress files.

## Command Interface

```
mur project remove [path]
```

- `path` — Path to the project (optional, defaults to current directory `pwd()`)
- No special flags needed — simple positional argument
- Must match the same path resolution as `mur project index` / `mur project status`

## Behavior

### Path Resolution

Same as existing subcommands (`status`, `index`):
1. If `path` is provided, expand `~` via `expand_tilde()` then `canonicalize()`
2. If omitted, use `std::env::current_dir()`
3. Extract `project_name` from directory name via `project_name_from_path()`

### Index Resolution (two-step)

1. **Direct by name**: Construct the expected `.lance` path from `project_name` and delete if it exists
2. **Fallback by path**: Scan `discover_all_indexes()` for a matching `meta.project_path` (handles renamed directories or previously existing indexes with different names)

### Cleanup

Delete all files under `~/.mur/indexes/codebase/` for the matched project:

| File/Dir | Condition |
|----------|-----------|
| `{name}.lance/` (directory) | Exists → `remove_dir_all` |
| `{name}.meta.json` | Exists → `remove_file` |
| `{name}.lock` | Exists → `remove_file` |
| `{name}.progress.json` | Exists → `remove_file` |

Only fails if filesystem operations fail (permissions, locked files).

### Output

```
$ mur project remove
Removed index for 'my-project' at /Users/me/Projects/my-project

$ mur project remove ~/Projects/nonexistent
Error: No index found for /Users/me/Projects/nonexistent
```

## Implementation

### Files to modify (mur-core crate)

| File | Change |
|------|--------|
| `src/cli/actions.rs` | Add `Remove { path: Option<String> }` variant to `ProjectAction` |
| `src/dispatch.rs` | Match `ProjectAction::Remove { path }` → `cmd_project_remove(path)` |
| `src/cmd/project.rs` | New `cmd_project_remove()` function |
| `src/codebase/mod.rs` | New `CodebaseIndex::delete_index()` method |

### `CodebaseIndex::delete_index()`

```rust
pub fn delete_index(&self) -> Result<()> {
    if self.lance_path.exists() {
        std::fs::remove_dir_all(&self.lance_path)?;
    }
    for path in [self.meta_path(), self.lock_path(), self.progress_path()] {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}
```

### `cmd_project_remove()`

```rust
pub fn cmd_project_remove(path: Option<String>) -> Result<()> {
    let project_path = resolve_path(path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    if index.lance_path().exists() {
        index.delete_index()?;
    } else {
        // Fallback: scan all indexes for matching project_path
        let found = discover_all_indexes().into_iter()
            .find(|d| d.project_path.as_deref()
                .map(|p| canonicalize_or(p) == project_path)
                .unwrap_or(false));
        match found {
            Some(entry) => {
                CodebaseIndex::new(&entry.name, &project_path).delete_index()?;
            }
            None => anyhow::bail!("No index found for {}", project_path.display()),
        }
    }
    println!("Removed index for '{}' at {}", project_name, project_path.display());
    Ok(())
}
```

### `ProjectAction::Remove` (clap)

```rust
/// Remove an indexed project
Remove {
    /// Path to the project (defaults to current directory)
    path: Option<String>,
},
```

## Error Cases

| Condition | Behavior |
|-----------|----------|
| No index exists for path/name | Print error with `anyhow::bail!`, list available projects |
| `.lance` dir exists but meta is missing | Delete what exists, report success |
| Permission denied on filesystem | Propagate IO error |
| `.lance` dir is locked (background indexing) | Delete anyway (lock is advisory; kill the background process first if running) |

## Testing

- Unit test for `delete_index()` on a fake project (create temp `.lance` dir + meta, delete, verify)
- Unit test for `discover_all_indexes()` fallback path matching
- Manual test: `mur project index`, `mur project remove`, verify with `mur project list`
