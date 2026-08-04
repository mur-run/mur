use anyhow::Result;
use chrono::Utc;
use mur_common::Signal;
use std::path::{Path, PathBuf};

/// Atomic file-based queue for Signal values pending server flush.
///
/// Files are named `YYYY-MM-DDTHH-MM-SS-{uuid}.yaml` and ordered by that prefix.
/// Writes use temp-then-rename for atomicity. Flushed items move to `.flushed/`
/// for audit (caller decides retention).
pub struct Outbox {
    dir: PathBuf,
}

impl Outbox {
    /// Open an outbox rooted at the given directory (creates it if missing).
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Open the default outbox at `$HOME/.mur/outbox/`.
    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME directory"))?;
        Self::new(home.join(".mur/outbox"))
    }

    /// Serialize a signal to YAML and persist atomically. Returns the final path.
    #[allow(dead_code)]
    pub fn write(&self, signal: &Signal) -> Result<PathBuf> {
        let ts = Utc::now().format("%Y-%m-%dT%H-%M-%S");
        let name = format!("{}-{}.yaml", ts, signal.id);
        let final_path = self.dir.join(&name);
        let tmp_path = self.dir.join(format!(".{}.tmp", name));
        let yaml = serde_yaml::to_string(signal)?;
        std::fs::write(&tmp_path, yaml)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(final_path)
    }

    /// List pending signal files, sorted by filename (= chronological).
    /// Hidden files (`.tmp`, `.flushed/` contents) are skipped.
    pub fn list_pending(&self) -> Result<Vec<PathBuf>> {
        let mut items: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("yaml")
                    && !p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(|n| n.starts_with('.'))
                    && p.is_file()
            })
            .collect();
        items.sort();
        Ok(items)
    }

    /// Move a previously-pending file into `.flushed/` (kept for audit).
    pub fn mark_flushed(&self, path: &Path) -> Result<()> {
        let flushed_dir = self.dir.join(".flushed");
        std::fs::create_dir_all(&flushed_dir)?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("path has no filename: {}", path.display()))?;
        std::fs::rename(path, flushed_dir.join(name))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind, SignalTarget};
    use tempfile::tempdir;
    use uuid::Uuid;

    fn sample_signal() -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::CommanderDaemon,
                native_id: "svc-1".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: "test-p".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
            sig: None,
            key_version: 0,
        }
    }

    #[test]
    fn write_creates_file_and_list_returns_it() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let s = sample_signal();
        let path = ob.write(&s).unwrap();
        assert!(path.exists(), "file should exist after write");
        assert!(path.starts_with(dir.path()));
        let pending = ob.list_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0], path);
    }

    #[test]
    fn list_pending_is_sorted_by_filename() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        // Write a few signals; sleep tiny amounts to guarantee different timestamps.
        let _p1 = ob.write(&sample_signal()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let _p2 = ob.write(&sample_signal()).unwrap();
        let pending = ob.list_pending().unwrap();
        assert_eq!(pending.len(), 2);
        // Sorted ascending by filename = chronological
        let mut sorted = pending.clone();
        sorted.sort();
        assert_eq!(pending, sorted);
    }

    #[test]
    fn mark_flushed_moves_to_subdir() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let p = ob.write(&sample_signal()).unwrap();
        ob.mark_flushed(&p).unwrap();
        assert!(!p.exists(), "original file should be gone");
        let flushed_dir = dir.path().join(".flushed");
        assert!(flushed_dir.exists());
        assert_eq!(flushed_dir.read_dir().unwrap().count(), 1);
        assert_eq!(
            ob.list_pending().unwrap().len(),
            0,
            ".flushed items not counted as pending"
        );
    }

    #[test]
    fn list_pending_ignores_tmp_and_flushed() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let p = ob.write(&sample_signal()).unwrap();
        // Simulate a tmp file left over from interrupted write
        std::fs::write(dir.path().join(".interrupted.yaml.tmp"), "junk").unwrap();
        // And a flushed subdir with a file
        ob.mark_flushed(&p).unwrap();
        let _p2 = ob.write(&sample_signal()).unwrap();
        let pending = ob.list_pending().unwrap();
        assert_eq!(pending.len(), 1, "only the new file should be pending");
    }

    #[test]
    fn write_is_atomic_no_partial_files_on_disk() {
        let dir = tempdir().unwrap();
        let ob = Outbox::new(dir.path()).unwrap();
        let _ = ob.write(&sample_signal()).unwrap();
        // No .tmp file should remain after successful write
        let leftover_tmps: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("tmp"))
            .collect();
        assert!(leftover_tmps.is_empty(), "no .tmp leftovers expected");
    }
}
