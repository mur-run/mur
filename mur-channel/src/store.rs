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
    fn events_path(&self, id: &str) -> PathBuf {
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

    pub fn load_events(&self, id: &str) -> Result<Vec<ChannelEvent>> {
        let path = self.events_path(id);
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<ChannelEvent>(l).ok())
            .collect())
    }

    /// Append one event under an advisory lock so `seq` stays monotonic across
    /// processes (the Hub and the CLI may append concurrently). Returns the
    /// event with its assigned `seq` and timestamp.
    pub fn append_event(
        &self,
        id: &str,
        actor: ChannelActor,
        kind: EventKind,
        payload: serde_json::Value,
        idempotency_key: Option<String>,
    ) -> Result<ChannelEvent> {
        let dir = self.channel_dir(id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = self.events_path(id);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        file.lock_exclusive().context("lock events file")?;

        // Compute next seq from the existing log tail (held under the lock).
        let next_seq = self.load_events(id)?.last().map(|e| e.seq + 1).unwrap_or(0);

        let ev = ChannelEvent {
            seq: next_seq,
            ts: Utc::now(),
            actor,
            kind,
            payload,
            idempotency_key,
        };
        let line = serde_json::to_string(&ev).context("serialize event")?;
        writeln!(file, "{line}").with_context(|| format!("write {}", path.display()))?;
        FileExt::unlock(&file).ok();
        Ok(ev)
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
            )
            .unwrap();
        let e1 = store
            .append_event(
                "c1",
                ChannelActor::Agent { id: "qa".into() },
                EventKind::Message,
                serde_json::json!({"text":"yo"}),
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
