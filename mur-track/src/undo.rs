//! Per-turn undo snapshots (design spec §4.1 step 4): the P0 degenerate form
//! of the edit ledger. Before a track is promoted, every path the turn
//! changed has its *before* bytes stored content-addressed under
//! `<agent_home>/edits/cas/<sha256>` and a manifest written to
//! `<agent_home>/edits/turns/<turn>.json`. P1 lifts each manifest entry into
//! an `edit.applied` channel event by attaching the hashes already here.
//!
//! Undo is last-write-wins in reverse: it compares each entry's `after` hash
//! with the file now on disk so the caller can name what has moved on since
//! the promote *before* overwriting it. It has no concurrency detection and
//! cannot itself be undone — both are P1's `edit.reverted`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Directory under the agent home that holds the CAS and the manifests.
pub const EDITS_DIR: &str = "edits";
const CAS_DIR: &str = "cas";
const TURNS_DIR: &str = "turns";

/// What the turn did to one path, as seen at promote time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Existed before, exists after with different bytes — undo restores `before`.
    Modified,
    /// Existed before, gone after — undo restores `before`.
    Deleted,
    /// Absent before, exists after — undo removes it.
    Added,
    /// Not snapshotted (symlink, directory, or the redactor fired). Undo
    /// reports it and leaves it alone.
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Project-relative path.
    pub path: PathBuf,
    pub kind: EntryKind,
    /// SHA-256 (hex) of the bytes before the turn; a CAS key when `kind`
    /// is `Modified` or `Deleted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// SHA-256 (hex) of the bytes the promote wrote; absent when `Deleted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Why a `Skipped` entry was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnManifest {
    pub turn: String,
    /// Absolute project root the track was promoted into.
    pub project: PathBuf,
    /// RFC 3339 UTC.
    pub promoted_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undone_at: Option<String>,
    pub entries: Vec<Entry>,
}

impl TurnManifest {
    /// Entries undo would act on (everything but `Skipped`).
    pub fn restorable(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.kind != EntryKind::Skipped)
    }
}

/// A path whose bytes on disk no longer match what the promote wrote, so
/// undo would overwrite work done after the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub path: PathBuf,
    pub kind: EntryKind,
}

/// What an undo did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UndoReport {
    pub restored: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, String)>,
}

/// `<agent_home>/edits`: the CAS plus one manifest per promoted turn.
#[derive(Debug, Clone)]
pub struct UndoStore {
    root: PathBuf,
}

impl UndoStore {
    pub fn new(agent_home: &Path) -> Self {
        Self {
            root: agent_home.join(EDITS_DIR),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn cas(&self, hash: &str) -> PathBuf {
        self.root.join(CAS_DIR).join(hash)
    }

    /// A turn id becomes a file name; one that could climb out of
    /// `turns/` (a CLI argument is user input) is mapped to a name that
    /// cannot exist rather than followed.
    fn manifest_path(&self, turn: &str) -> PathBuf {
        let safe = !turn.is_empty() && !turn.contains(['/', '\\']) && turn != "." && turn != "..";
        let name = if safe { turn } else { ".invalid" };
        self.root.join(TURNS_DIR).join(format!("{name}.json"))
    }

    /// Record the before-state of `files` (project-relative, as returned by
    /// `TurnTrack::diff_files`) with `track` holding the after-state. Call
    /// *before* promote; the project still has the before bytes then.
    pub fn snapshot(
        &self,
        turn: &str,
        project: &Path,
        track: &Path,
        files: &[PathBuf],
    ) -> Result<TurnManifest> {
        std::fs::create_dir_all(self.root.join(CAS_DIR))?;
        std::fs::create_dir_all(self.root.join(TURNS_DIR))?;
        let mut entries = Vec::with_capacity(files.len());
        for rel in files {
            entries.push(self.snapshot_one(rel, &project.join(rel), &track.join(rel))?);
        }
        let manifest = TurnManifest {
            turn: turn.to_string(),
            project: project.to_path_buf(),
            promoted_at: chrono::Utc::now().to_rfc3339(),
            undone_at: None,
            entries,
        };
        self.write_manifest(&manifest)?;
        Ok(manifest)
    }

    fn snapshot_one(&self, rel: &Path, before: &Path, after: &Path) -> Result<Entry> {
        let skip = |reason: &str| Entry {
            path: rel.to_path_buf(),
            kind: EntryKind::Skipped,
            before: None,
            after: None,
            reason: Some(reason.to_string()),
        };
        let before_bytes = match regular_file(before)? {
            Shape::Absent => None,
            Shape::Regular(b) => Some(b),
            Shape::Other(what) => return Ok(skip(&format!("{what} before the turn"))),
        };
        let after_hash = match regular_file(after)? {
            Shape::Absent => None,
            Shape::Regular(b) => Some(hex_sha256(&b)),
            Shape::Other(what) => return Ok(skip(&format!("{what} after the turn"))),
        };
        let before_hash = match &before_bytes {
            Some(b) => {
                // §5.2: the CAS refuses bytes the redactor would change.
                if let Ok(text) = std::str::from_utf8(b)
                    && matches!(
                        mur_common::redact::redact_secrets(text),
                        std::borrow::Cow::Owned(_)
                    )
                {
                    return Ok(skip("redactor fired on the before bytes"));
                }
                Some(self.put(b)?)
            }
            None => None,
        };
        let kind = match (&before_hash, &after_hash) {
            (Some(_), Some(_)) => EntryKind::Modified,
            (Some(_), None) => EntryKind::Deleted,
            (None, Some(_)) => EntryKind::Added,
            (None, None) => return Ok(skip("absent on both sides")),
        };
        Ok(Entry {
            path: rel.to_path_buf(),
            kind,
            before: before_hash,
            after: after_hash,
            reason: None,
        })
    }

    /// Store bytes content-addressed; returns the hex key.
    fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = hex_sha256(bytes);
        let dst = self.cas(&hash);
        if !dst.exists() {
            atomic_write(&dst, bytes)?;
        }
        Ok(hash)
    }

    fn write_manifest(&self, m: &TurnManifest) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(m)?;
        atomic_write(&self.manifest_path(&m.turn), &bytes)
    }

    pub fn load(&self, turn: &str) -> Result<Option<TurnManifest>> {
        let p = self.manifest_path(turn);
        match std::fs::read(&p) {
            Ok(b) => Ok(Some(
                serde_json::from_slice(&b).with_context(|| format!("parse {}", p.display()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
        }
    }

    /// Every manifest, newest promote first.
    pub fn list(&self) -> Result<Vec<TurnManifest>> {
        let dir = self.root.join(TURNS_DIR);
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
        };
        for ent in rd {
            let p = ent?.path();
            if p.extension().is_some_and(|x| x == "json")
                && let Ok(m) = serde_json::from_slice::<TurnManifest>(&std::fs::read(&p)?)
            {
                out.push(m);
            }
        }
        out.sort_by(|a, b| b.promoted_at.cmp(&a.promoted_at));
        Ok(out)
    }

    /// Paths whose on-disk bytes differ from what the promote wrote.
    pub fn drift(&self, m: &TurnManifest) -> Result<Vec<Drift>> {
        let mut out = Vec::new();
        for e in m.restorable() {
            let now = match regular_file(&m.project.join(&e.path))? {
                Shape::Absent => None,
                Shape::Regular(b) => Some(hex_sha256(&b)),
                Shape::Other(_) => Some(String::new()),
            };
            if now != e.after {
                out.push(Drift {
                    path: e.path.clone(),
                    kind: e.kind,
                });
            }
        }
        Ok(out)
    }

    /// Put the project back to the turn's before-state. Last write wins:
    /// the caller is expected to have shown [`Self::drift`] first. A second
    /// undo of the same turn is refused.
    pub fn undo(&self, m: &mut TurnManifest) -> Result<UndoReport> {
        if let Some(at) = &m.undone_at {
            bail!("turn {} was already undone at {at}", m.turn);
        }
        let mut report = UndoReport::default();
        for e in &m.entries {
            let dst = m.project.join(&e.path);
            match e.kind {
                EntryKind::Modified | EntryKind::Deleted => {
                    let key = e.before.as_deref().unwrap_or_default();
                    let bytes = std::fs::read(self.cas(key))
                        .with_context(|| format!("CAS object {key} for {}", e.path.display()))?;
                    atomic_write(&dst, &bytes)?;
                    report.restored.push(e.path.clone());
                }
                EntryKind::Added => {
                    match std::fs::remove_file(&dst) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => {
                            return Err(err).with_context(|| format!("remove {}", dst.display()));
                        }
                    }
                    report.removed.push(e.path.clone());
                }
                EntryKind::Skipped => report
                    .skipped
                    .push((e.path.clone(), e.reason.clone().unwrap_or_default())),
            }
        }
        m.undone_at = Some(chrono::Utc::now().to_rfc3339());
        self.write_manifest(m)?;
        Ok(report)
    }
}

enum Shape {
    Absent,
    Regular(Vec<u8>),
    Other(&'static str),
}

fn regular_file(p: &Path) -> Result<Shape> {
    let meta = match std::fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Shape::Absent),
        Err(e) => return Err(e).with_context(|| format!("stat {}", p.display())),
    };
    if meta.file_type().is_symlink() {
        return Ok(Shape::Other("symlink"));
    }
    if meta.is_dir() {
        return Ok(Shape::Other("directory"));
    }
    Ok(Shape::Regular(
        std::fs::read(p).with_context(|| format!("read {}", p.display()))?,
    ))
}

fn hex_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Temp file + rename in the target's directory, so a crash never leaves a
/// half-written file where a whole one was.
fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target.parent().context("target has no parent")?;
    std::fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, target).with_context(|| format!("rename into {}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, UndoStore) {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let track = tmp.path().join("track");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::create_dir_all(track.join("src")).unwrap();
        let store = UndoStore::new(&tmp.path().join("agent"));
        (tmp, project, track, store)
    }

    fn promote_like(project: &Path, track: &Path, rel: &str) {
        let src = track.join(rel);
        let dst = project.join(rel);
        if src.exists() {
            std::fs::copy(src, dst).unwrap();
        } else if dst.exists() {
            std::fs::remove_file(dst).unwrap();
        }
    }

    #[test]
    fn snapshot_classifies_and_undo_restores_every_byte() {
        let (_tmp, project, track, store) = fixture();
        std::fs::write(project.join("src/a.rs"), "old a").unwrap();
        std::fs::write(track.join("src/a.rs"), "new a").unwrap();
        std::fs::write(project.join("src/gone.rs"), "was here").unwrap();
        std::fs::write(track.join("src/new.rs"), "fresh").unwrap();
        let files: Vec<PathBuf> = ["src/a.rs", "src/gone.rs", "src/new.rs"]
            .iter()
            .map(PathBuf::from)
            .collect();

        let m = store.snapshot("t1", &project, &track, &files).unwrap();
        let kinds: Vec<_> = m.entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [EntryKind::Modified, EntryKind::Deleted, EntryKind::Added]
        );
        for f in &files {
            promote_like(&project, &track, f.to_str().unwrap());
        }
        assert_eq!(
            std::fs::read_to_string(project.join("src/a.rs")).unwrap(),
            "new a"
        );
        assert!(store.drift(&m).unwrap().is_empty());

        let mut m = store.load("t1").unwrap().unwrap();
        let r = store.undo(&mut m).unwrap();
        assert_eq!(r.restored.len(), 2);
        assert_eq!(r.removed, [PathBuf::from("src/new.rs")]);
        assert_eq!(
            std::fs::read_to_string(project.join("src/a.rs")).unwrap(),
            "old a"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("src/gone.rs")).unwrap(),
            "was here"
        );
        assert!(!project.join("src/new.rs").exists());
        assert!(m.undone_at.is_some());
        assert!(store.undo(&mut m).is_err(), "second undo must be refused");
        assert!(store.load("../../etc/passwd").unwrap().is_none());
    }

    #[test]
    fn drift_names_files_changed_after_promote() {
        let (_tmp, project, track, store) = fixture();
        std::fs::write(project.join("src/a.rs"), "old").unwrap();
        std::fs::write(track.join("src/a.rs"), "new").unwrap();
        let files = vec![PathBuf::from("src/a.rs")];
        let m = store.snapshot("t2", &project, &track, &files).unwrap();
        promote_like(&project, &track, "src/a.rs");
        std::fs::write(project.join("src/a.rs"), "user edited after").unwrap();
        let d = store.drift(&m).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].path, PathBuf::from("src/a.rs"));
    }

    #[test]
    fn cas_dedups_and_symlinks_are_skipped() {
        let (_tmp, project, track, store) = fixture();
        std::fs::write(project.join("src/a.rs"), "same").unwrap();
        std::fs::write(project.join("src/b.rs"), "same").unwrap();
        std::fs::write(track.join("src/a.rs"), "x").unwrap();
        std::fs::write(track.join("src/b.rs"), "y").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("a.rs", track.join("src/link")).unwrap();
        let mut files = vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")];
        #[cfg(unix)]
        files.push(PathBuf::from("src/link"));
        let m = store.snapshot("t3", &project, &track, &files).unwrap();
        let cas = std::fs::read_dir(store.root().join(CAS_DIR))
            .unwrap()
            .count();
        assert_eq!(cas, 1, "identical before bytes stored once");
        #[cfg(unix)]
        assert_eq!(m.entries[2].kind, EntryKind::Skipped);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn redactor_hit_is_skipped_not_stored() {
        let (_tmp, project, track, store) = fixture();
        std::fs::write(
            project.join("src/key.pem"),
            "-----BEGIN RSA PRIVATE KEY-----\nabc\n",
        )
        .unwrap();
        std::fs::write(track.join("src/key.pem"), "clean").unwrap();
        let m = store
            .snapshot("t4", &project, &track, &[PathBuf::from("src/key.pem")])
            .unwrap();
        assert_eq!(m.entries[0].kind, EntryKind::Skipped);
        assert_eq!(
            std::fs::read_dir(store.root().join(CAS_DIR))
                .unwrap()
                .count(),
            0
        );
        let mut m = store.load("t4").unwrap().unwrap();
        let r = store.undo(&mut m).unwrap();
        assert_eq!(r.skipped.len(), 1);
    }
}
