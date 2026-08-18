use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use mur_common::channel::{Channel, ChannelActor, ChannelEvent, EventKind};

/// Event-sourced store rooted at `<mur_home>/channels/`.
pub struct ChannelStore {
    root: PathBuf,
}

impl ChannelStore {
    pub fn new(mur_home: &Path) -> Self {
        Self {
            root: mur_home.join("channels"),
        }
    }

    fn channel_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
    /// Path of a channel's append-only log. Public so a reader can cheaply
    /// gate on its length instead of re-parsing an unchanged log.
    pub fn events_path(&self, id: &str) -> PathBuf {
        self.channel_dir(id).join("events.jsonl")
    }
    fn manifest_path(&self, id: &str) -> PathBuf {
        self.channel_dir(id).join("channel.yaml")
    }

    /// Create the channel directory and write its initial manifest.
    pub fn create(&self, channel: &Channel) -> Result<()> {
        let dir = self.channel_dir(&channel.id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create channel dir {}", dir.display()))?;
        self.save_manifest(channel)
    }

    /// Atomic manifest write (temp file + rename), matching the YamlStore idiom.
    pub fn save_manifest(&self, channel: &Channel) -> Result<()> {
        let path = self.manifest_path(&channel.id);
        let yaml = serde_yaml::to_string(channel).context("serialize channel manifest")?;
        let tmp = path.with_extension("yaml.tmp");
        fs::write(&tmp, &yaml).with_context(|| format!("write {}", tmp.display()))?;
        fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
        Ok(())
    }

    pub fn load_manifest(&self, id: &str) -> Result<Channel> {
        let path = self.manifest_path(id);
        let s = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_yaml::from_str(&s).with_context(|| format!("parse {}", path.display()))
    }

    /// Read a channel's events. A line that fails to parse is skipped so one
    /// damaged record cannot make a whole channel unreadable — but it is
    /// COUNTED and WARNED with its line number, because a dropped line is a
    /// dropped event and every fold above this (run rebuild, pending HITL
    /// gates, the rail, `mur channel show`) would otherwise be unable to tell
    /// "that never happened" from "I could not read it". Losing a
    /// `HitlResponse` silently makes an approved gate read as still waiting.
    pub fn load_events(&self, id: &str) -> Result<Vec<ChannelEvent>> {
        let path = self.events_path(id);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let mut events = Vec::new();
        let mut damaged = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChannelEvent>(line) {
                Ok(ev) => events.push(ev),
                // 1-based: matches what an editor or `sed -n` shows.
                Err(_) => damaged.push(idx + 1),
            }
        }
        if !damaged.is_empty() {
            tracing::warn!(
                channel_id = %id,
                path = %path.display(),
                dropped = damaged.len(),
                lines = ?damaged,
                "unparseable event line(s) skipped — the events they carried are missing from every fold of this channel"
            );
        }
        Ok(events)
    }

    /// Append one event under an advisory lock so `seq` stays monotonic across
    /// processes (the Hub and the CLI may append concurrently). Returns the
    /// event with its assigned `seq` and timestamp.
    #[allow(clippy::too_many_arguments)]
    pub fn append_event(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
        sig: Option<String>,
        key_version: Option<u32>,
    ) -> Result<ChannelEvent> {
        let dir = self.channel_dir(id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.events_path(id);

        // Serialize concurrent appends (CLI + Hub may write the same channel) via
        // a SIDECAR lock file — never lock events.jsonl itself. On Windows OS file
        // locks are mandatory (not advisory like flock on macOS/Linux), so holding
        // a lock on the data file blocks our own reads/writes of it (os error 33).
        // Locking a separate file gives cross-process mutual exclusion while the
        // data file stays freely readable/writable on every platform.
        let lock_path = dir.join("events.lock");
        let lock = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        lock.lock_exclusive().context("lock channel events")?;

        // Dedup: if this idempotency_key already exists in the log, return the
        // prior event unchanged (exactly-once for crash-reruns; v3c). Done under
        // the lock so a concurrent writer can't slip a duplicate in between.
        if let Some(key) = idempotency_key.as_deref()
            && let Some(existing) = self
                .load_events(id)?
                .into_iter()
                .find(|e| e.idempotency_key.as_deref() == Some(key))
        {
            FileExt::unlock(&lock).ok();
            return Ok(existing);
        }

        // Compute next seq from the existing (unlocked) log, held under the lock.
        let next_seq = self.load_events(id)?.last().map(|e| e.seq + 1).unwrap_or(0);

        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
            sig,
            key_version,
        };
        let line = serde_json::to_string(&ev).context("serialize event")?;
        let mut data = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        // ONE `write_all` of the record plus its terminator, not `writeln!`.
        // `File` is unbuffered, so `writeln!` is two `write(2)` calls: a process
        // that dies between them leaves a line with no newline, and the next
        // append (O_APPEND, starting at EOF) glues its JSON onto the orphan —
        // losing BOTH events, silently, in every reader. A single write cannot
        // be torn that way by a crash between syscalls.
        let mut record = line.into_bytes();
        record.push(b'\n');
        data.write_all(&record)
            .with_context(|| format!("write {}", path.display()))?;
        FileExt::unlock(&lock).ok();
        Ok(ev)
    }

    /// Delete the channel directory entirely. Idempotent — no error if already absent.
    pub fn delete(&self, id: &str) -> Result<()> {
        let dir = self.channel_dir(id);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| format!("delete channel dir {}", dir.display())),
        }
    }

    /// List every channel id present on disk.
    pub fn list_ids(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut ids = Vec::new();
        for entry in
            fs::read_dir(&self.root).with_context(|| format!("read {}", self.root.display()))?
        {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{ChannelState, Goal};
    use tempfile::TempDir;

    fn sample_channel(id: &str) -> Channel {
        let now = Utc::now();
        Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: id.to_string(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: None,
            owner: ChannelActor::Human { name: "me".into() },
            participants: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn create_then_load_manifest() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let got = store.load_manifest("c1").unwrap();
        assert_eq!(got.id, "c1");
        assert_eq!(got.state, ChannelState::Working);
    }

    /// A damaged line must not take a whole channel down with it, and must not
    /// leave without saying so. This is the shape a crash mid-append produces:
    /// a line with no terminator, glued to the next event's JSON, so BOTH
    /// records are unparseable — silently, under every fold in the product.
    #[test]
    fn a_damaged_line_is_skipped_but_warned_with_its_line_number() {
        use std::io::Write as _;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Message,
                serde_json::json!({"n": 1}),
                None,
                None,
                None,
            )
            .unwrap();

        // Line 2: two records glued together — what an interrupted append
        // leaves behind once the next append writes at EOF.
        {
            let path = store.events_path("c1");
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(br#"{"seq":1,"seq":2}{"seq":3}"#).unwrap();
            f.write_all(b"\n").unwrap();
        }

        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Message,
                serde_json::json!({"n": 3}),
                None,
                None,
                None,
            )
            .unwrap();

        let capture = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::WARN)
            .finish();
        let events =
            tracing::subscriber::with_default(subscriber, || store.load_events("c1")).unwrap();

        assert_eq!(
            events.len(),
            2,
            "the healthy events on either side of the damage must still load"
        );

        let logged = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logged.contains("c1"),
            "the warning must name the channel: {logged}"
        );
        // `lines=[2]`, not a bare "2": the timestamp and the temp path are full
        // of digits, so a loose match would pass with no line number at all.
        assert!(
            logged.contains("dropped\u{1b}[0m\u{1b}[2m=\u{1b}[0m1") || logged.contains("dropped=1"),
            "the warning must say how many lines were dropped: {logged}"
        );
        assert!(
            logged.contains("[2]"),
            "the warning must name the damaged line number: {logged}"
        );
    }

    #[test]
    fn append_assigns_monotonic_seq() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let e0 = store
            .append_event(
                "c1",
                ChannelActor::Human { name: "me".into() },
                EventKind::Message,
                serde_json::json!({"text":"hi"}),
                None,
                None,
                None,
            )
            .unwrap();
        let e1 = store
            .append_event(
                "c1",
                ChannelActor::Agent { id: "qa".into() },
                EventKind::Message,
                serde_json::json!({"text":"yo"}),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(e0.seq, 0);
        assert_eq!(e1.seq, 1);
        let all = store.load_events("c1").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].payload["text"], "yo");
    }

    #[test]
    fn append_dedups_on_idempotency_key() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let e0 = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::ToolResult,
                serde_json::json!({"x":1}),
                Some("k1".into()),
                None,
                None,
            )
            .unwrap();
        // Same key again → returns the EXISTING event, does not append a 2nd row.
        let e0b = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::ToolResult,
                serde_json::json!({"x":2}),
                Some("k1".into()),
                None,
                None,
            )
            .unwrap();
        assert_eq!(e0.seq, e0b.seq, "same idempotency_key → same event");
        assert_eq!(e0b.payload["x"], 1, "first write wins; second is ignored");
        assert_eq!(
            store.load_events("c1").unwrap().len(),
            1,
            "no duplicate row"
        );
        // A None key never dedups.
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .unwrap();
        store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Note,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(store.load_events("c1").unwrap().len(), 3);
    }

    #[test]
    fn list_ids_returns_created() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("a")).unwrap();
        store.create(&sample_channel("b")).unwrap();
        let mut ids = store.list_ids().unwrap();
        ids.sort();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn save_manifest_overwrites_existing() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let mut ch = sample_channel("c1");
        store.create(&ch).unwrap();
        ch.state = ChannelState::Completed;
        ch.title = "updated".into();
        store.save_manifest(&ch).unwrap();
        let got = store.load_manifest("c1").unwrap();
        assert_eq!(got.state, ChannelState::Completed);
        assert_eq!(got.title, "updated");
    }
}
