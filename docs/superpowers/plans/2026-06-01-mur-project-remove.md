# `mur project remove` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `mur project remove [path]` to delete a project's indexed data (LanceDB, meta, lock, progress files).

**Architecture:** Four files changed in `mur-core`: add `Remove` variant to `ProjectAction` clap enum, dispatch it, implement `cmd_project_remove()` that resolves path → project name → deletes files via a new `CodebaseIndex::delete_index()` method.

**Tech Stack:** Rust, clap, std::fs

---

### Task 1: Add `Remove` variant to ProjectAction enum

**Files:**
- Modify: `mur-core/src/cli/actions.rs:768-785`

- [ ] **Step 1: Add Remove variant after List**

Open `mur-core/src/cli/actions.rs` and after the `List` variant, add:

```rust
    /// Remove an indexed project
    Remove {
        /// Path to the project (defaults to current directory)
        path: Option<String>,
    },
```

- [ ] **Step 2: Verify clap parses correctly**

Run: `cd ~/Projects/mur && cargo check -p mur-core`
Expected: No errors.

---

### Task 2: Add `delete_index()` method to CodebaseIndex

**Files:**
- Modify: `mur-core/src/codebase/mod.rs:642-645`

- [ ] **Step 1: Add delete_index method after lance_path()**

Open `mur-core/src/codebase/mod.rs`. After the `lance_path()` method (line 642-644), add:

```rust
    /// Delete all index data for this project (LanceDB dir, meta, lock, progress).
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

- [ ] **Step 2: Verify it compiles**

Run: `cd ~/Projects/mur && cargo check -p mur-core`
Expected: No errors.

---

### Task 3: Implement `cmd_project_remove()`

**Files:**
- Modify: `mur-core/src/cmd/project.rs:331-348`

- [ ] **Step 1: Add `cmd_project_remove` after `cmd_project_list`**

Open `mur-core/src/cmd/project.rs`. After `cmd_project_list()` (ends around line 348), add:

```rust
pub(crate) fn cmd_project_remove(path: Option<String>) -> Result<()> {
    let project_path = match &path {
        Some(p) => expand_tilde(p),
        None => std::env::current_dir()?,
    };
    let project_path = project_path.canonicalize().unwrap_or(project_path);
    let project_name = project_name_from_path(&project_path);
    let index = CodebaseIndex::new(&project_name, &project_path);

    if index.lance_path().exists() {
        index.delete_index()?;
        println!("Removed index for '{}' at {}", project_name, project_path.display());
        return Ok(());
    }

    // Fallback: scan all indexes for matching project_path (handles renamed dirs)
    let indexes = discover_all_indexes();
    let found = indexes.iter().find(|d| {
        d.project_path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).canonicalize().ok())
            .map(|p| p == project_path)
            .unwrap_or(false)
    });

    match found {
        Some(entry) => {
            let fallback = CodebaseIndex::new(&entry.name, &project_path);
            fallback.delete_index()?;
            println!("Removed index for '{}' at {}", entry.name, project_path.display());
            Ok(())
        }
        None => {
            // Show available projects to help the user
            if indexes.is_empty() {
                anyhow::bail!(
                    "No index found for '{}'.\n  No projects are currently indexed. Run `mur project index` first.",
                    project_path.display()
                );
            }
            anyhow::bail!(
                "No index found for '{}'.\n  Indexed projects:\n{}",
                project_path.display(),
                indexes
                    .iter()
                    .map(|d| format!("    {}  ({})", d.name, d.project_path.as_deref().unwrap_or("?")))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd ~/Projects/mur && cargo check -p mur-core`
Expected: No errors.

---

### Task 4: Wire dispatch for `ProjectAction::Remove`

**Files:**
- Modify: `mur-core/src/dispatch.rs:993`

- [ ] **Step 1: Add Remove dispatch branch**

Open `mur-core/src/dispatch.rs`. After the `ProjectAction::List` dispatch line (line 993), add a comma and:

```rust
            ProjectAction::Remove { path } => cmd::project::cmd_project_remove(path)?,
```

The full match block should look like:

```rust
            ProjectAction::Search {
                query,
                project,
                limit,
                json,
            } => cmd::project::cmd_project_search(query, project, limit, json).await?,
            ProjectAction::Status { path } => cmd::project::cmd_project_status(path).await?,
            ProjectAction::List => cmd::project::cmd_project_list()?,
            ProjectAction::Remove { path } => cmd::project::cmd_project_remove(path)?,
        },
```

- [ ] **Step 2: Full compile check**

Run: `cd ~/Projects/mur && cargo check -p mur-core`
Expected: No errors.

---

### Task 5: Manual verification

- [ ] **Step 1: Build release binary**

Run: `cd ~/Projects/mur && cargo build -p mur-core --release`

- [ ] **Step 2: Test with an existing project**

```bash
cd ~/Projects/mur-commander
# First check it's indexed
mur project list
# Then remove it
mur project remove
# Verify it's gone
mur project list
```

Expected: `mur project remove` prints "Removed index for 'mur-commander' at ..." and `mur project list` no longer shows it.

- [ ] **Step 3: Test with explicit path**

```bash
mur project remove ~/Projects/mur-commander
```

Expected: Error about no index found (already removed in step 2).

- [ ] **Step 4: Test error case for non-existent project**

```bash
mur project remove /nonexistent/path
```

Expected: Error with "No index found for ..." and list of indexed projects.

- [ ] **Step 5: Re-index to restore**

```bash
cd ~/Projects/mur-commander
mur project index
mur project list
```

Expected: Project re-appears in list.
