# Parallel Tracks P2 — ZFS Unified Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a ZFS-backed `ParallelBackend` that creates parallel tracks via instant block-clones, making track creation ~16× faster (5ms vs 80ms) and cutting disk usage 80–90% on systems with ZFS available.

**Architecture:** Three new components — `ZfsNativeBackend` (Linux/FreeBSD direct ZFS CLI), `ZfsSocketBackend` (sends JSON over a Unix socket to a `mur-zfs-agent` daemon inside a Linux VM on macOS/Windows), and an updated `detect_backend` that prefers ZFS. Shared protocol types live in `mur-common`. `mur-zfs-agent` is a new minimal workspace binary (~200 lines) that runs inside OrbStack/Lima/WSL2 and exposes a Unix socket.

**Tech Stack:** Rust, `serde_json` + `anyhow` + `dirs` (all already in workspace), `std::os::unix::net` (std). Zero new crate dependencies.

## Global Constraints

- Rust edition 2024; `let`-chains stable.
- Zero new crate dependencies.
- `ParallelBackend` trait signature is fixed (defined in `mur-core/src/parallel/backend/mod.rs`):
  - `fn create_track(&self, name: &str) -> Result<PathBuf>`
  - `fn base_snapshot(&self, track: &Path) -> Result<String>`
  - `fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>>`
  - `fn promote(&self, track: &Path, target: &Path) -> Result<()>`
  - `fn destroy(&self, track: &Path) -> Result<()>`
- ZFS-specific tests gated on `#[cfg(any(target_os = "linux", target_os = "freebsd"))]` plus a runtime `zfs_cli_available()` check — must never fail on macOS/Windows CI.
- Socket protocol: newline-delimited JSON (one serialized value per line, no framing).
- `mur-zfs-agent` added to workspace `members` in root `Cargo.toml`.
- Files ≤ 800 lines.
- All `#[tauri::command]` pattern does not apply here — this is CLI/library code only.
- Brand: "MUR" uppercase in user-visible strings; `mur` lowercase in code identifiers.

---

## File Map

| File | Change |
|---|---|
| `mur-common/src/zfs_protocol.rs` | **Create** — `ZfsRequest` / `ZfsResponse` shared types |
| `mur-common/src/lib.rs` | **Modify** — `pub mod zfs_protocol;` |
| `mur-zfs-agent/Cargo.toml` | **Create** — new workspace crate |
| `mur-zfs-agent/src/main.rs` | **Create** — Unix socket listener + request dispatch |
| `mur-zfs-agent/src/zfs.rs` | **Create** — ZFS CLI wrappers |
| `Cargo.toml` | **Modify** — add `"mur-zfs-agent"` to `workspace.members` |
| `mur-core/src/parallel/backend/zfs_native.rs` | **Create** — `ZfsNativeBackend` (Linux/FreeBSD) |
| `mur-core/src/parallel/backend/zfs_socket.rs` | **Create** — `ZfsSocketBackend` + connector fns |
| `mur-core/src/parallel/backend/mod.rs` | **Modify** — add `pub mod zfs_native; pub mod zfs_socket;` + re-exports |
| `mur-core/src/parallel/backend/detect.rs` | **Modify** — ZFS → socket → git worktree detection chain |
| `scripts/gate4_zfs_latency.sh` | **Create** — benchmark create_track latency vs. git worktrees |

---

## Task 1: Shared protocol types in mur-common

**Files:**
- Create: `mur-common/src/zfs_protocol.rs`
- Modify: `mur-common/src/lib.rs`

**Interfaces:**
- Produces: `ZfsRequest` enum (used by Tasks 2, 3, 4, 5)
- Produces: `ZfsResponse` enum (used by Tasks 2, 3, 4, 5)

- [ ] **Step 1: Write failing serde roundtrip tests**

Create `mur-common/src/zfs_protocol.rs` with just the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_track_roundtrip() {
        let req = ZfsRequest::CreateTrack {
            base: "/pool/data/project".into(),
            name: "track-a".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ZfsRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsRequest::CreateTrack { .. }));
    }

    #[test]
    fn error_response_roundtrip() {
        let resp = ZfsResponse::Error { message: "zfs: command not found".into() };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ZfsResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsResponse::Error { .. }));
    }

    #[test]
    fn all_request_variants_serialize() {
        let reqs = vec![
            ZfsRequest::CreateTrack { base: "/p".into(), name: "t".into() },
            ZfsRequest::Snapshot { track: "/p/t".into(), label: "base".into() },
            ZfsRequest::DiffFiles { track: "/p/t".into(), since: "mur-base".into() },
            ZfsRequest::Destroy { track: "/p/t".into() },
        ];
        for req in reqs {
            let json = serde_json::to_string(&req).unwrap();
            let _back: ZfsRequest = serde_json::from_str(&json).unwrap();
        }
    }
}
```

Run: `ORT_STRATEGY=download cargo test -p mur-common zfs_protocol 2>&1 | tail -5`
Expected: FAIL — `cannot find module 'zfs_protocol'`

- [ ] **Step 2: Implement the types**

Replace `mur-common/src/zfs_protocol.rs` with the full implementation:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Request sent from mur-core (host) to mur-zfs-agent (inside VM).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ZfsRequest {
    CreateTrack { base: PathBuf, name: String },
    Snapshot     { track: PathBuf, label: String },
    DiffFiles    { track: PathBuf, since: String },
    Destroy      { track: PathBuf },
}

/// Response sent from mur-zfs-agent back to mur-core.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ZfsResponse {
    /// CreateTrack succeeded — `path` is the mountpoint of the new clone.
    Track   { path: PathBuf },
    /// Snapshot succeeded — `snap_id` is the full snapshot name (e.g. `pool/ds@label`).
    Snap    { snap_id: String },
    /// DiffFiles succeeded — `paths` are repo-relative changed paths.
    Files   { paths: Vec<PathBuf> },
    /// Operation succeeded with no output (Destroy).
    Ok,
    /// Operation failed — `message` is the error string.
    Error   { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_track_roundtrip() {
        let req = ZfsRequest::CreateTrack {
            base: "/pool/data/project".into(),
            name: "track-a".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: ZfsRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsRequest::CreateTrack { .. }));
    }

    #[test]
    fn error_response_roundtrip() {
        let resp = ZfsResponse::Error { message: "zfs: command not found".into() };
        let json = serde_json::to_string(&resp).unwrap();
        let back: ZfsResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ZfsResponse::Error { .. }));
    }

    #[test]
    fn all_request_variants_serialize() {
        let reqs = vec![
            ZfsRequest::CreateTrack { base: "/p".into(), name: "t".into() },
            ZfsRequest::Snapshot { track: "/p/t".into(), label: "base".into() },
            ZfsRequest::DiffFiles { track: "/p/t".into(), since: "mur-base".into() },
            ZfsRequest::Destroy { track: "/p/t".into() },
        ];
        for req in reqs {
            let json = serde_json::to_string(&req).unwrap();
            let _back: ZfsRequest = serde_json::from_str(&json).unwrap();
        }
    }
}
```

- [ ] **Step 3: Register the module**

Add to `mur-common/src/lib.rs` (after the existing `pub mod` list):

```rust
pub mod zfs_protocol;
```

- [ ] **Step 4: Run tests**

```bash
ORT_STRATEGY=download cargo test -p mur-common zfs_protocol 2>&1 | tail -5
```

Expected: 3 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/zfs_protocol.rs mur-common/src/lib.rs
git commit -m "feat(parallel/p2): ZFS protocol types in mur-common"
```

---

## Task 2: ZfsNativeBackend

**Files:**
- Create: `mur-core/src/parallel/backend/zfs_native.rs`
- Modify: `mur-core/src/parallel/backend/mod.rs`

**Interfaces:**
- Consumes: `ParallelBackend` trait from `mur-core/src/parallel/backend/mod.rs`
- Produces: `pub struct ZfsNativeBackend { project_root: PathBuf }` + `impl ParallelBackend`
- Produces: `pub fn zfs_cli_available() -> bool` (used by Task 5)
- Produces: `pub fn is_on_zfs_pool(p: &Path) -> bool` (used by Task 5)
- Produces: `pub fn parse_dataset(output: &str) -> Option<String>` (internal, tested)
- Produces: `pub fn parse_diff_output(output: &str, track_mount: &Path) -> Vec<PathBuf>` (internal, tested)

- [ ] **Step 1: Write failing tests for pure parsing functions**

```rust
// in mur-core/src/parallel/backend/zfs_native.rs — tests section only first

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_parses_pool_path() {
        assert_eq!(
            parse_dataset("pool/data/project\n"),
            Some("pool/data/project".to_string())
        );
    }

    #[test]
    fn dataset_rejects_empty_and_dash() {
        assert_eq!(parse_dataset(""), None);
        assert_eq!(parse_dataset("-\n"), None);
    }

    #[test]
    fn diff_output_extracts_modified_and_added() {
        let out = "M\t/pool/data/project/src/main.rs\n\
                   +\t/pool/data/project/src/new.rs\n\
                   -\t/pool/data/project/old.rs\n";
        let paths = parse_diff_output(out, Path::new("/pool/data/project"));
        // M and + included; - (deleted) also included so promote skips missing files
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p == Path::new("src/main.rs")));
        assert!(paths.iter().any(|p| p == Path::new("src/new.rs")));
    }

    #[test]
    fn diff_output_strips_mount_prefix() {
        let out = "M\t/mnt/project/src/lib.rs\n";
        let paths = parse_diff_output(out, Path::new("/mnt/project"));
        assert_eq!(paths, vec![PathBuf::from("src/lib.rs")]);
    }
}
```

Run: `ORT_STRATEGY=download cargo test -p mur-core parallel::backend::zfs_native 2>&1 | tail -5`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement `zfs_native.rs`**

```rust
#![allow(dead_code, unused_imports)]
use super::ParallelBackend;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ZfsNativeBackend {
    pub project_root: PathBuf,
}

impl ZfsNativeBackend {
    pub fn new(project_root: PathBuf) -> Self {
        Self { project_root }
    }
}

pub fn zfs_cli_available() -> bool {
    Command::new("zfs").arg("version")
        .output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn is_on_zfs_pool(path: &Path) -> bool {
    dataset_for_path(path).is_ok()
}

pub fn parse_dataset(output: &str) -> Option<String> {
    let s = output.trim();
    if s.is_empty() || s == "-" { None } else { Some(s.to_string()) }
}

/// Parse `zfs diff` output and strip the `track_mount` prefix, returning RELATIVE paths.
/// (The agent's `parse_zfs_diff_output` returns ABSOLUTE paths — intentionally different.)
pub fn parse_diff_output(output: &str, track_mount: &Path) -> Vec<PathBuf> {
    output.lines()
        .filter_map(|l| l.split_once('\t').map(|(_, p)| PathBuf::from(p)))
        .filter_map(|p| p.strip_prefix(track_mount).ok().map(|r| r.to_path_buf()))
        .collect()
}

fn dataset_for_path(path: &Path) -> Result<String> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "name"])
        .arg(path)
        .output()
        .context("zfs get name")?;
    parse_dataset(&String::from_utf8_lossy(&out.stdout))
        .context("path is not on a ZFS dataset")
}

fn dataset_snapshot(dataset: &str, label: &str) -> Result<String> {
    let snap = format!("{dataset}@{label}");
    let st = Command::new("zfs").args(["snapshot", &snap])
        .status().context("zfs snapshot")?;
    if !st.success() { bail!("zfs snapshot {snap} failed"); }
    Ok(snap)
}

fn dataset_clone(snap: &str, target_dataset: &str) -> Result<PathBuf> {
    let st = Command::new("zfs").args(["clone", snap, target_dataset])
        .status().context("zfs clone")?;
    if !st.success() { bail!("zfs clone failed: {snap} → {target_dataset}"); }
    mountpoint_of(target_dataset)
}

fn mountpoint_of(dataset: &str) -> Result<PathBuf> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output().context("zfs get mountpoint")?;
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

impl ParallelBackend for ZfsNativeBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        let base_dataset = dataset_for_path(&self.project_root)?;
        let snap = dataset_snapshot(&base_dataset, &format!("mur-parallel-base-{name}"))?;
        // Scope tracks under the project's own dataset to avoid cross-project collisions.
        let track_dataset = format!("{base_dataset}/mur-tracks/{name}");
        dataset_clone(&snap, &track_dataset)
    }

    fn base_snapshot(&self, track: &Path) -> Result<String> {
        let ds = dataset_for_path(track)?;
        dataset_snapshot(&ds, "mur-base")
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        let ds = dataset_for_path(track)?;
        let since = format!("{ds}@{since_snapshot}");
        let out = Command::new("zfs").args(["diff", &since, &ds])
            .output().context("zfs diff")?;
        Ok(parse_diff_output(&String::from_utf8_lossy(&out.stdout), track))
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        for rel in self.diff_files(track, "mur-base")? {
            let src = track.join(&rel);
            let dst = target.join(&rel);
            if src.is_file() {
                if let Some(p) = dst.parent() { std::fs::create_dir_all(p)?; }
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        let ds = dataset_for_path(track)?;
        let st = Command::new("zfs").args(["destroy", "-r", &ds])
            .status().context("zfs destroy")?;
        if !st.success() { bail!("zfs destroy {ds} failed"); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dataset_parses_pool_path() {
        assert_eq!(
            parse_dataset("pool/data/project\n"),
            Some("pool/data/project".to_string())
        );
    }

    #[test]
    fn dataset_rejects_empty_and_dash() {
        assert_eq!(parse_dataset(""), None);
        assert_eq!(parse_dataset("-\n"), None);
    }

    #[test]
    fn diff_output_extracts_modified_and_added() {
        let out = "M\t/pool/data/project/src/main.rs\n\
                   +\t/pool/data/project/src/new.rs\n\
                   -\t/pool/data/project/old.rs\n";
        let paths = parse_diff_output(out, Path::new("/pool/data/project"));
        assert_eq!(paths.len(), 3);
        assert!(paths.iter().any(|p| p == Path::new("src/main.rs")));
        assert!(paths.iter().any(|p| p == Path::new("src/new.rs")));
    }

    #[test]
    fn diff_output_strips_mount_prefix() {
        let out = "M\t/mnt/project/src/lib.rs\n";
        let paths = parse_diff_output(out, Path::new("/mnt/project"));
        assert_eq!(paths, vec![PathBuf::from("src/lib.rs")]);
    }
}
```

- [ ] **Step 3: Register module in `mod.rs`**

Add to `mur-core/src/parallel/backend/mod.rs`:

```rust
pub mod zfs_native;
pub use zfs_native::ZfsNativeBackend;
```

- [ ] **Step 4: Run tests**

```bash
ORT_STRATEGY=download cargo test -p mur-core parallel::backend::zfs_native 2>&1 | tail -10
```

Expected: 4 tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/backend/zfs_native.rs mur-core/src/parallel/backend/mod.rs
git commit -m "feat(parallel/p2): ZfsNativeBackend — direct ZFS CLI backend for Linux/FreeBSD"
```

---

## Task 3: mur-zfs-agent binary

**Files:**
- Create: `mur-zfs-agent/Cargo.toml`
- Create: `mur-zfs-agent/src/main.rs`
- Create: `mur-zfs-agent/src/zfs.rs`
- Modify: `Cargo.toml` (workspace root)

**Interfaces:**
- Consumes: `mur_common::zfs_protocol::{ZfsRequest, ZfsResponse}` (Task 1)
- Produces: `mur-zfs-agent` binary — listens on Unix socket at path `$MUR_ZFS_SOCKET` (default `/run/mur-zfs-agent.sock`), reads newline-delimited `ZfsRequest` JSON, writes newline-delimited `ZfsResponse` JSON.

- [ ] **Step 1: Write failing tests for ZFS CLI wrappers**

Create `mur-zfs-agent/src/zfs.rs` with just the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diff_empty() {
        assert!(parse_zfs_diff_output("").is_empty());
    }

    #[test]
    fn parse_diff_extracts_paths() {
        let out = "M\t/data/project/src/lib.rs\nM\t/data/project/Cargo.toml\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));
    }

    #[test]
    fn parse_diff_ignores_blank_lines() {
        let out = "\nM\t/data/file.rs\n\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 1);
    }
}
```

Run: `ORT_STRATEGY=download cargo test -p mur-zfs-agent 2>&1 | tail -5`
Expected: FAIL — crate does not exist yet.

- [ ] **Step 2: Create `mur-zfs-agent/Cargo.toml`**

```toml
[package]
name = "mur-zfs-agent"
version.workspace = true
edition.workspace = true
description = "MUR ZFS socket agent — runs inside a Linux environment, exposes a Unix socket for ZFS operations"

[[bin]]
name = "mur-zfs-agent"
path = "src/main.rs"

[dependencies]
mur-common  = { path = "../mur-common" }
serde_json  = { workspace = true }
anyhow      = { workspace = true }
```

- [ ] **Step 3: Add `mur-zfs-agent` to workspace `Cargo.toml`**

In the root `Cargo.toml`, add `"mur-zfs-agent"` to the `members` list:

```toml
members = [
    "mur-common",
    "mur-core",
    "mur-agent-runtime",
    "mur-daemon",
    "mur-gui-core",
    "mur-agent-launcher",
    "mur-mcp-server",
    "mur-compress",
    "mur-channel",
    "mur-mobile-sdk",
    "mur-zfs-agent",
]
```

- [ ] **Step 4: Implement `mur-zfs-agent/src/zfs.rs`**

```rust
use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

pub fn dataset_for_path(path: &std::path::Path) -> Result<String> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "name"])
        .arg(path)
        .output()
        .context("zfs get name")?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "-" { bail!("path not on ZFS: {path:?}"); }
    Ok(s)
}

pub fn zfs_clone(snap: &str, dataset: &str) -> Result<PathBuf> {
    let st = Command::new("zfs").args(["clone", snap, dataset])
        .status().context("zfs clone")?;
    if !st.success() { bail!("zfs clone failed"); }
    mountpoint_of(dataset)
}

pub fn zfs_snapshot(dataset: &str, label: &str) -> Result<String> {
    let snap = format!("{dataset}@{label}");
    let st = Command::new("zfs").args(["snapshot", &snap])
        .status().context("zfs snapshot")?;
    if !st.success() { bail!("zfs snapshot failed: {snap}"); }
    Ok(snap)
}

pub fn zfs_diff(dataset: &str, since_snap: &str) -> Result<Vec<PathBuf>> {
    let since = format!("{dataset}@{since_snap}");
    let out = Command::new("zfs").args(["diff", &since, dataset])
        .output().context("zfs diff")?;
    Ok(parse_zfs_diff_output(&String::from_utf8_lossy(&out.stdout)))
}

pub fn zfs_destroy(dataset: &str) -> Result<()> {
    let st = Command::new("zfs").args(["destroy", "-r", dataset])
        .status().context("zfs destroy")?;
    if !st.success() { bail!("zfs destroy failed: {dataset}"); }
    Ok(())
}

fn mountpoint_of(dataset: &str) -> Result<PathBuf> {
    let out = Command::new("zfs")
        .args(["get", "-H", "-o", "value", "mountpoint", dataset])
        .output().context("zfs get mountpoint")?;
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Parse `zfs diff` output into a list of ABSOLUTE paths (no prefix stripping).
/// Unlike `zfs_native::parse_diff_output`, which strips the mount prefix and returns
/// relative paths, this returns absolute paths as given by ZFS — the agent works in
/// absolute terms; the host-side backend strips the prefix if needed.
/// Format: `<change-type>\t<path>` (M = modified, + = added, - = deleted, R = renamed).
pub fn parse_zfs_diff_output(output: &str) -> Vec<PathBuf> {
    output.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_once('\t').map(|(_, p)| PathBuf::from(p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_diff_empty() {
        assert!(parse_zfs_diff_output("").is_empty());
    }

    #[test]
    fn parse_diff_extracts_paths() {
        let out = "M\t/data/project/src/lib.rs\nM\t/data/project/Cargo.toml\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("src/lib.rs")));
    }

    #[test]
    fn parse_diff_ignores_blank_lines() {
        let out = "\nM\t/data/file.rs\n\n";
        let paths = parse_zfs_diff_output(out);
        assert_eq!(paths.len(), 1);
    }
}
```

- [ ] **Step 5: Implement `mur-zfs-agent/src/main.rs`**

```rust
use anyhow::Result;
use mur_common::zfs_protocol::{ZfsRequest, ZfsResponse};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

mod zfs;

fn handle(req: ZfsRequest) -> ZfsResponse {
    match req {
        ZfsRequest::CreateTrack { base, name } => {
            let result = zfs::dataset_for_path(&base).and_then(|ds| {
                let snap = zfs::zfs_snapshot(&ds, &format!("mur-parallel-base-{name}"))?;
                // Scope under project dataset — same convention as ZfsNativeBackend.
                let track_ds = format!("{ds}/mur-tracks/{name}");
                zfs::zfs_clone(&snap, &track_ds)
            });
            match result {
                Ok(path)  => ZfsResponse::Track { path },
                Err(e)    => ZfsResponse::Error { message: e.to_string() },
            }
        }
        ZfsRequest::Snapshot { track, label } => {
            match zfs::dataset_for_path(&track).and_then(|ds| zfs::zfs_snapshot(&ds, &label)) {
                Ok(snap_id) => ZfsResponse::Snap { snap_id },
                Err(e)      => ZfsResponse::Error { message: e.to_string() },
            }
        }
        ZfsRequest::DiffFiles { track, since } => {
            match zfs::dataset_for_path(&track).and_then(|ds| zfs::zfs_diff(&ds, &since)) {
                Ok(paths) => ZfsResponse::Files { paths },
                Err(e)    => ZfsResponse::Error { message: e.to_string() },
            }
        }
        ZfsRequest::Destroy { track } => {
            match zfs::dataset_for_path(&track).and_then(|ds| zfs::zfs_destroy(&ds)) {
                Ok(())  => ZfsResponse::Ok,
                Err(e)  => ZfsResponse::Error { message: e.to_string() },
            }
        }
    }
}

fn main() -> Result<()> {
    let socket_path = std::env::var("MUR_ZFS_SOCKET")
        .unwrap_or_else(|_| "/run/mur-zfs-agent.sock".to_string());
    let _ = std::fs::remove_file(&socket_path); // remove stale socket
    let listener = UnixListener::bind(&socket_path)?;
    eprintln!("mur-zfs-agent listening on {socket_path}");

    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut writer = stream.try_clone()?;
        let reader = BufReader::new(&mut stream);
        for line in reader.lines() {
            let resp = match serde_json::from_str::<ZfsRequest>(&line?) {
                Ok(req)  => handle(req),
                Err(e)   => ZfsResponse::Error { message: format!("parse error: {e}") },
            };
            let mut out = serde_json::to_string(&resp)?;
            out.push('\n');
            writer.write_all(out.as_bytes())?;
        }
    }
    Ok(())
}
```

- [ ] **Step 6: Run tests**

```bash
ORT_STRATEGY=download cargo test -p mur-zfs-agent 2>&1 | tail -10
```

Expected: 3 tests in `zfs` module pass.

- [ ] **Step 7: Build check**

```bash
ORT_STRATEGY=download cargo build -p mur-zfs-agent 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 8: Commit**

```bash
git add mur-zfs-agent/ Cargo.toml
git commit -m "feat(parallel/p2): mur-zfs-agent — Unix socket daemon for ZFS operations in Linux VMs"
```

---

## Task 4: ZfsSocketBackend + connectors

**Files:**
- Create: `mur-core/src/parallel/backend/zfs_socket.rs`
- Modify: `mur-core/src/parallel/backend/mod.rs`

**Interfaces:**
- Consumes: `mur_common::zfs_protocol::{ZfsRequest, ZfsResponse}` (Task 1)
- Consumes: `ParallelBackend` trait
- Produces: `pub struct ZfsSocketBackend { socket_path: PathBuf, project_root: PathBuf }` (used by Task 5)
- Produces: `pub fn connect_orbstack_socket() -> Result<PathBuf>` (used by Task 5)
- Produces: `pub fn connect_lima_socket(name: &str) -> Result<PathBuf>` (used by Task 5)
- Produces: `#[cfg(windows)] pub fn connect_wsl2_socket() -> Result<PathBuf>` (used by Task 5)

Note on socket paths:
- OrbStack forwards guest sockets to `~/.orbstack/run/sockets/` on the host — run `mur-zfs-agent` inside OrbStack and it appears at `~/.orbstack/run/sockets/mur-zfs-agent.sock`.
- Lima forwards guest sockets via `limactl shell <name>` — the socket at `/run/mur-zfs-agent.sock` inside the VM is accessible at `~/.lima/<name>/sock/mur-zfs-agent.sock` on the host when Lima socket forwarding is configured.
- WSL2: uses `AF_UNIX` sockets on Windows 10 1903+ — the guest socket maps to a host-side path.

- [ ] **Step 1: Write failing test using a mock socket server**

```rust
// in mur-core/src/parallel/backend/zfs_socket.rs — tests section only first

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::zfs_protocol::ZfsResponse;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn mock_agent(socket_path: &Path) {
        let listener = UnixListener::bind(socket_path).unwrap();
        std::thread::spawn(move || {
            // Accept one connection and always respond ZfsResponse::Ok
            if let Ok((stream, _)) = listener.accept() {
                let mut w = stream.try_clone().unwrap();
                let r = BufReader::new(stream);
                for line in r.lines().flatten() {
                    let _ = line; // consume request
                    let mut resp = serde_json::to_string(&ZfsResponse::Ok).unwrap();
                    resp.push('\n');
                    let _ = w.write_all(resp.as_bytes());
                }
            }
        });
    }

    #[test]
    fn socket_backend_destroy_ok() {
        let sock = std::path::PathBuf::from(
            format!("/tmp/mur-zfs-test-{}.sock", std::process::id())
        );
        let _ = std::fs::remove_file(&sock);
        mock_agent(&sock);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let backend = ZfsSocketBackend::new(sock.clone(), "/tmp/fake-project".into());
        let result = backend.destroy(std::path::Path::new("/tmp/fake-track"));
        let _ = std::fs::remove_file(&sock);
        assert!(result.is_ok());
    }
}
```

Run: `ORT_STRATEGY=download cargo test -p mur-core parallel::backend::zfs_socket 2>&1 | tail -5`
Expected: FAIL — module not found.

- [ ] **Step 2: Implement `zfs_socket.rs`**

```rust
#![allow(dead_code, unused_imports)]
use super::ParallelBackend;
use anyhow::{Context, Result, bail};
use mur_common::zfs_protocol::{ZfsRequest, ZfsResponse};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub struct ZfsSocketBackend {
    pub socket_path: PathBuf,
    /// Project root as seen by the VM (OrbStack/Lima mount same path as host on macOS).
    pub project_root: PathBuf,
}

impl ZfsSocketBackend {
    pub fn new(socket_path: PathBuf, project_root: PathBuf) -> Self {
        Self { socket_path, project_root }
    }

    fn call(&self, req: ZfsRequest) -> Result<ZfsResponse> {
        let stream = UnixStream::connect(&self.socket_path)
            .context("connect to mur-zfs-agent socket")?;
        let mut writer = stream.try_clone()?;
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        drop(writer);
        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line)?;
        serde_json::from_str(resp_line.trim()).context("parse ZfsResponse")
    }
}

impl ParallelBackend for ZfsSocketBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        match self.call(ZfsRequest::CreateTrack {
            base: self.project_root.clone(),
            name: name.into(),
        })? {
            ZfsResponse::Track { path } => Ok(path),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn base_snapshot(&self, track: &Path) -> Result<String> {
        match self.call(ZfsRequest::Snapshot {
            track: track.into(),
            label: "mur-base".into(),
        })? {
            ZfsResponse::Snap { snap_id } => Ok(snap_id),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        match self.call(ZfsRequest::DiffFiles {
            track: track.into(),
            since: since_snapshot.into(),
        })? {
            ZfsResponse::Files { paths } => Ok(paths),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        // File copy runs on the host, not inside the VM.
        // diff_files returns paths as seen by the VM; OrbStack/Lima mount the project root
        // at the same absolute path as the host, so the paths are directly usable here.
        // ponytail: breaks if the VM mounts the host FS at a different path prefix.
        for rel in self.diff_files(track, "mur-base")? {
            let src = track.join(&rel);
            let dst = target.join(&rel);
            if src.is_file() {
                if let Some(p) = dst.parent() { std::fs::create_dir_all(p)?; }
                std::fs::copy(&src, &dst)?;
            }
        }
        Ok(())
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        match self.call(ZfsRequest::Destroy { track: track.into() })? {
            ZfsResponse::Ok => Ok(()),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }
}

/// OrbStack forwards guest sockets to `~/.orbstack/run/sockets/` on the host.
/// Start `mur-zfs-agent` inside OrbStack: `orb run -- mur-zfs-agent`
pub fn connect_orbstack_socket() -> Result<PathBuf> {
    let path = dirs::home_dir()
        .context("no home dir")?
        .join(".orbstack/run/sockets/mur-zfs-agent.sock");
    if path.exists() { Ok(path) } else { bail!("OrbStack socket not found: {path:?}") }
}

/// Lima forwards guest sockets via socket forwarding configuration.
/// Add to the Lima VM config: `portForwards: [{guestSocket: "/run/mur-zfs-agent.sock"}]`
pub fn connect_lima_socket(name: &str) -> Result<PathBuf> {
    let path = dirs::home_dir()
        .context("no home dir")?
        .join(format!(".lima/{name}/sock/mur-zfs-agent.sock"));
    if path.exists() { Ok(path) } else { bail!("Lima socket not found: {path:?}") }
}

/// WSL2 AF_UNIX socket forwarding (Windows 10 1903+).
/// The guest socket at `/run/mur-zfs-agent.sock` must be explicitly forwarded to the host.
///
/// IMPLEMENTER NOTE: The path below is a placeholder. The actual host-side path for
/// a forwarded WSL2 AF_UNIX socket is NOT documented in a single authoritative place.
/// Before shipping Windows support, research: https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/
/// and test with `socat` inside WSL2 to confirm the host-visible path. Alternatively,
/// consider AF_VSOCK (`VM_SOCKETS`) as a more stable cross-version approach.
#[cfg(windows)]
pub fn connect_wsl2_socket() -> Result<PathBuf> {
    // ponytail: exact WSL2 AF_UNIX host-side path varies by Windows version and WSL distro.
    // Upgrade path: use AF_VSOCK or a named pipe when AF_UNIX forwarding is unavailable.
    let appdata = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA not set")?;
    let path = PathBuf::from(appdata).join("Temp\\mur-zfs-agent.sock");
    if path.exists() { Ok(path) } else { bail!("WSL2 socket not found: {path:?}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::zfs_protocol::ZfsResponse;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn mock_agent(socket_path: &Path) {
        let listener = UnixListener::bind(socket_path).unwrap();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut w = stream.try_clone().unwrap();
                let r = BufReader::new(stream);
                for line in r.lines().flatten() {
                    let _ = line;
                    let mut resp = serde_json::to_string(&ZfsResponse::Ok).unwrap();
                    resp.push('\n');
                    let _ = w.write_all(resp.as_bytes());
                }
            }
        });
    }

    #[test]
    fn socket_backend_destroy_ok() {
        let sock = PathBuf::from(format!("/tmp/mur-zfs-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        mock_agent(&sock);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let backend = ZfsSocketBackend::new(sock.clone(), "/tmp/fake-project".into());
        let result = backend.destroy(Path::new("/tmp/fake-track"));
        let _ = std::fs::remove_file(&sock);
        assert!(result.is_ok());
    }
}
```

- [ ] **Step 3: Register in `mod.rs`**

Add to `mur-core/src/parallel/backend/mod.rs`:

```rust
pub mod zfs_socket;
pub use zfs_socket::ZfsSocketBackend;
```

- [ ] **Step 4: Run tests**

```bash
ORT_STRATEGY=download cargo test -p mur-core parallel::backend::zfs_socket 2>&1 | tail -10
```

Expected: `socket_backend_destroy_ok ... ok`

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/parallel/backend/zfs_socket.rs mur-core/src/parallel/backend/mod.rs
git commit -m "feat(parallel/p2): ZfsSocketBackend + OrbStack/Lima/WSL2 connectors"
```

---

## Task 5: Updated detect_backend

**Files:**
- Modify: `mur-core/src/parallel/backend/detect.rs`

**Interfaces:**
- Consumes: `ZfsNativeBackend`, `zfs_cli_available`, `is_on_zfs_pool` (Task 2)
- Consumes: `ZfsSocketBackend`, `connect_orbstack_socket`, `connect_lima_socket` (Task 4)
- Produces: updated `detect_backend(project: &Path) -> Box<dyn ParallelBackend>` — prefers ZFS

Detection order: ZfsNative (Linux/FreeBSD) → OrbStack socket (macOS) → Lima socket (any) → WSL2 socket (Windows) → GitWorktreeBackend (always available).

- [ ] **Step 1: Write the test**

```rust
// Confirm the function never panics regardless of what backends are available.
#[test]
fn detect_backend_never_panics() {
    let backend = detect_backend(std::path::Path::new("."));
    // Exercise — errors are expected on a dev machine without ZFS tracks
    let _ = backend.diff_files(std::path::Path::new("/nonexistent"), "snap");
}
```

Run: `ORT_STRATEGY=download cargo test -p mur-core parallel::backend::detect 2>&1 | tail -5`
Expected: currently passes (existing test). Will still pass after modification.

- [ ] **Step 2: Replace `detect.rs` with the full implementation**

```rust
#![allow(dead_code, unused_imports)]
use super::{
    GitWorktreeBackend, ParallelBackend,
    git_worktree::find_git_root,
    zfs_native::{ZfsNativeBackend, is_on_zfs_pool, zfs_cli_available},
    zfs_socket::{ZfsSocketBackend, connect_lima_socket, connect_orbstack_socket},
};
use std::path::Path;

/// Returns the best available `ParallelBackend` for `project`.
///
/// Detection order:
/// 1. ZFS native  — Linux/FreeBSD with `zfs` CLI + project on a ZFS pool
/// 2. OrbStack    — macOS; mur-zfs-agent socket forwarded by OrbStack
/// 3. Lima        — macOS/Linux; Lima VM named "mur-zfs" with socket forwarding
/// 4. WSL2        — Windows; mur-zfs-agent socket forwarded from WSL2 distro
/// 5. GitWorktree — always available, zero extra deps
pub fn detect_backend(project: &Path) -> Box<dyn ParallelBackend> {
    // 1. ZFS native (Linux/FreeBSD only)
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    if zfs_cli_available() && is_on_zfs_pool(project) {
        return Box::new(ZfsNativeBackend::new(project.to_path_buf()));
    }

    // 2. OrbStack socket (macOS)
    #[cfg(target_os = "macos")]
    if let Ok(sock) = connect_orbstack_socket() {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 3. Lima "mur-zfs" instance socket (macOS and Linux)
    #[cfg(not(windows))]
    if let Ok(sock) = connect_lima_socket("mur-zfs") {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 4. WSL2 socket (Windows)
    #[cfg(windows)]
    if let Ok(sock) = super::zfs_socket::connect_wsl2_socket() {
        return Box::new(ZfsSocketBackend::new(sock, project.to_path_buf()));
    }

    // 5. Fallback: git worktrees (always available, zero deps)
    let root = find_git_root(project).unwrap_or_else(|| project.to_path_buf());
    Box::new(GitWorktreeBackend::new(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_backend_never_panics() {
        let backend = detect_backend(std::path::Path::new("."));
        let _ = backend.diff_files(std::path::Path::new("/nonexistent"), "snap");
    }
}
```

- [ ] **Step 3: Run all backend tests**

```bash
ORT_STRATEGY=download cargo test -p mur-core parallel::backend 2>&1 | tail -15
```

Expected: all `parallel::backend::*` tests pass.

- [ ] **Step 4: Full build check**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo build -p mur-core 2>&1 | tail -5
```

Expected: `Finished`.

- [ ] **Step 5: Run clippy**

```bash
ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo clippy -p mur-core -p mur-common -p mur-zfs-agent -- -D warnings 2>&1 | grep "^error" | head -10
```

Expected: no errors.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/parallel/backend/detect.rs
git commit -m "feat(parallel/p2): detect_backend — ZFS preferred over git worktrees when available"
```

---

## Task 6: Gate 4 validation

**Files:**
- Modify: `mur-core/src/parallel/backend/detect.rs` (add `#[ignore]` timing test)
- Create: `scripts/gate4_zfs_latency.sh` (wrapper that runs the test)

**Interfaces:**
- Consumes: `detect_backend()` → `ParallelBackend::create_track` / `destroy`
- Produces: printed mean latency; compares against Gate 4 thresholds

**Gate 4 pass criteria (from spec):**
- ZFS `create_track` mean ≤ 10ms (vs git worktrees ~80ms baseline)
- Disk usage for 10 tracks: ZFS ≤ 20% of git worktrees

- [ ] **Step 1: Add `#[ignore]` timing test to `detect.rs`**

Inside the existing `#[cfg(test)] mod tests` block in `mur-core/src/parallel/backend/detect.rs`, add:

```rust
/// Gate 4 latency benchmark. Run manually on a ZFS-equipped Linux machine:
///   ORT_STRATEGY=download cargo test -p mur-core \
///     "parallel::backend::detect::tests::bench_create_track" \
///     -- --ignored --nocapture
#[test]
#[ignore]
fn bench_create_track() {
    use std::time::Instant;
    let project = std::path::Path::new(".");
    let backend = detect_backend(project);
    let n = 10usize;
    let mut total = std::time::Duration::ZERO;
    for i in 0..n {
        let name = format!("gate4-bench-{i}");
        let start = Instant::now();
        let result = backend.create_track(&name);
        let elapsed = start.elapsed();
        if let Ok(track) = result {
            let _ = backend.destroy(&track);
        }
        total += elapsed;
        eprintln!("  iter {i}: {}ms", elapsed.as_millis());
    }
    let mean_ms = total.as_millis() / n as u128;
    eprintln!("Mean create_track: {mean_ms}ms");
    eprintln!("Gate 4 targets: ZFS ≤ 10ms / git worktrees ≤ 150ms");
}
```

Run it to confirm it compiles (the test is ignored so it won't actually run ZFS commands):

```bash
ORT_STRATEGY=download cargo test -p mur-core parallel::backend::detect -- --list
```

Expected: `tests::bench_create_track: test` appears in the output with `(test marked with #[ignore])`.

- [ ] **Step 2: Create gate4 wrapper script**

Create `scripts/gate4_zfs_latency.sh`:

```bash
#!/usr/bin/env bash
# Gate 4: ZFS vs git-worktree create_track latency.
# Run on a Linux machine with ZFS available and mur-zfs-agent running.
# Usage: bash scripts/gate4_zfs_latency.sh
# The test calls detect_backend() which picks ZFS automatically on a ZFS host.
set -euo pipefail

echo "=== Gate 4: create_track latency ==="
echo ""

ORT_STRATEGY=download cargo test -p mur-core \
    "parallel::backend::detect::tests::bench_create_track" \
    -- --ignored --nocapture 2>&1

echo ""
echo "Pass criteria: ZFS mean ≤ 10ms  |  git worktree mean ≤ 150ms"
```

- [ ] **Step 3: Make executable and commit**

```bash
chmod +x scripts/gate4_zfs_latency.sh
git add mur-core/src/parallel/backend/detect.rs scripts/gate4_zfs_latency.sh
git commit -m "feat(parallel/p2): Gate 4 ZFS latency validation via cargo test"
```

---

## Done

After all 6 tasks pass:

1. **Run Gate 4** on a Linux machine with ZFS and `mur-zfs-agent` running:
   ```bash
   bash scripts/gate4_zfs_latency.sh
   ```

2. **Tag the P2 release** once Gate 4 passes:
   ```bash
   git tag -a v-parallel-p2 -m "feat: parallel tracks P2 ZFS backend"
   ```

3. **Next plan — P1.5 Platform COW** (separate plan, separate phase):
   - APFS `cp -c` for macOS (GB-sized `target/` copies in milliseconds)
   - Btrfs `--reflink=always` for Linux
   - `tmutil localsnapshot` as pre-run safety net (no entitlement required)
   - Modify `mur-core/src/parallel/backend/git_worktree.rs:create_track` to call `copy_build_cache`

4. **Defer SmolVM to P2.5** — requires cross-compiling and bundling a mini VM binary; evaluate after measuring OrbStack/Lima adoption.

5. **Hub GUI diff viewer** (listed as P2 feature in spec) — separate UI plan. Shows the cherry-pick decision in a visual diff with per-function score badges.
