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
    /// Needed for the chain-head anchors, which live beside `channels/`
    /// rather than inside it so the sandbox grant on `channels/` cannot
    /// cover them.
    mur_home: PathBuf,
}

mod append;
mod chain;
mod load;

pub use append::{PreSealHook, PreSealInput, SealHook, SealHooks, SealInput};
pub use chain::{GENESIS_PREV_HASH, line_hash};
#[cfg(test)]
use load::check_absence_is_not_corruption;
pub use load::{AnchorMismatch, AnchorMismatchKind, DamageReport};

impl ChannelStore {
    pub fn new(mur_home: &Path) -> Self {
        Self {
            root: mur_home.join("channels"),
            mur_home: mur_home.to_path_buf(),
        }
    }

    /// The `mur_home` this store was opened on (anchors live under it).
    pub fn mur_home(&self) -> &Path {
        &self.mur_home
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

    /// Give `id` a manifest if it has none, so a channel that holds events is
    /// never invisible to the readers that ask for one. No-op when a manifest
    /// exists — including an unparseable one, which is a corruption to be
    /// reported, not silently replaced.
    ///
    /// The placeholder is honest about what it knows: the id, the schema
    /// version, and that nobody claimed ownership. Title and participants fill
    /// in through the normal paths (`add_participant`, a later `save_manifest`).
    fn ensure_manifest(&self, id: &str) -> Result<()> {
        if self.manifest_path(id).exists() {
            return Ok(());
        }
        let now = chrono::Utc::now();
        self.save_manifest(&Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: id.to_string(),
            title: id.to_string(),
            goal: mur_common::channel::Goal::default(),
            state: mur_common::channel::ChannelState::Working,
            purpose: None,
            owner: ChannelActor::System,
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
    }

    pub fn load_manifest(&self, id: &str) -> Result<Channel> {
        let path = self.manifest_path(id);
        let s = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_yaml::from_str(&s).with_context(|| format!("parse {}", path.display()))
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

    /// A channel whose manifest is gone — archived away, or never written
    /// because the directory was resurrected by an append — must not stay
    /// half-existent. Events kept landing in it while every reader that asks
    /// `load_manifest` (the fleet rail's existence check) said "channel
    /// unreadable" forever.
    #[test]
    fn appending_to_a_channel_with_no_manifest_restores_one() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("fleet-x")).unwrap();
        store
            .append_event(
                "fleet-x",
                ChannelActor::Human { name: "me".into() },
                EventKind::Message,
                serde_json::json!({"text": "first"}),
                None,
                None,
                None,
            )
            .unwrap();

        // What archiving did on a live machine: the manifest left, the events
        // stayed, and the next append resurrected the directory without one.
        std::fs::remove_file(store.manifest_path("fleet-x")).unwrap();
        assert!(store.load_manifest("fleet-x").is_err(), "precondition");

        store
            .append_event(
                "fleet-x",
                ChannelActor::Human { name: "me".into() },
                EventKind::Message,
                serde_json::json!({"text": "second"}),
                None,
                None,
                None,
            )
            .unwrap();

        let ch = store
            .load_manifest("fleet-x")
            .expect("the append must leave the channel readable");
        assert_eq!(ch.id, "fleet-x");
        assert_eq!(ch.v, mur_common::channel::CHANNEL_SCHEMA_VERSION);
        // Both events survive: healing the manifest must not touch the log.
        assert_eq!(store.load_events("fleet-x").unwrap().len(), 2);
    }

    /// The heal is write-if-absent: an existing manifest is never rewritten,
    /// or every append would clobber the title, owner and participants the
    /// real channel carries.
    #[test]
    fn appending_never_overwrites_an_existing_manifest() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let mut ch = sample_channel("c-keep");
        ch.title = "the real title".into();
        store.create(&ch).unwrap();
        store
            .append_event(
                "c-keep",
                ChannelActor::Human { name: "me".into() },
                EventKind::Message,
                serde_json::json!({"text": "hi"}),
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store.load_manifest("c-keep").unwrap().title,
            "the real title"
        );
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

    /// P4: `load_events` is UNCHANGED by the additive read API — same
    /// signature, same skip-and-`warn!` behaviour, same events, for a log
    /// that carries both an unparseable line and a badly-signed one
    /// (AC11f). This is the non-regression half; the next test checks what
    /// the new path reports for the SAME log.
    #[test]
    fn load_events_is_unchanged_by_the_additive_read_api() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let id = mur_common::identity::AgentIdentity::generate();
        let ev1 = store
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
        // A garbled line, exactly like the existing damaged-line test.
        {
            let path = store.events_path("c1");
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"{not json at all").unwrap();
            f.write_all(b"\n").unwrap();
        }
        // A well-formed but badly-SIGNED event (parses fine; signature fails).
        let payload = serde_json::json!({"n": 2});
        let bad_sig = crate::sign::sign_event(
            &mur_common::identity::AgentIdentity::generate(), // wrong key
            "c1",
            &ChannelActor::System,
            EventKind::Message,
            &payload,
            None,
        );
        let ev3 = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Message,
                payload,
                None,
                Some(bad_sig),
                Some(0),
            )
            .unwrap();

        let before = store.load_events("c1").unwrap();
        assert_eq!(before.len(), 2, "the garbled line is skipped, as today");
        assert_eq!(before[0].seq, ev1.seq);
        assert_eq!(before[1].seq, ev3.seq);

        let (after, _report) = store
            .load_events_with_damage("c1", &id.verifying_key_bytes(), false)
            .unwrap();
        assert_eq!(
            after.len(),
            before.len(),
            "the new path must return the SAME events for the same log"
        );
        assert_eq!(after[0].seq, before[0].seq);
        assert_eq!(after[1].seq, before[1].seq);
    }

    /// P4: the new path reports the garbled line's number and the
    /// badly-signed event, for the log `load_events` silently tolerates
    /// (AC11f). The damaged event is still RETURNED (report, not filter).
    #[test]
    fn load_events_with_damage_reports_unparseable_lines_and_bad_signatures() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let writer = mur_common::identity::AgentIdentity::generate();
        let good_sig_ev = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Message,
                serde_json::json!({"n": 1}),
                None,
                Some(crate::sign::sign_event(
                    &writer,
                    "c1",
                    &ChannelActor::System,
                    EventKind::Message,
                    &serde_json::json!({"n": 1}),
                    None,
                )),
                Some(0),
            )
            .unwrap();
        // Line 2: unparseable.
        {
            let path = store.events_path("c1");
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(b"not even json").unwrap();
            f.write_all(b"\n").unwrap();
        }
        // Line 3: parses, but signed by the WRONG key.
        let bad_payload = serde_json::json!({"n": 3});
        let forged_sig = crate::sign::sign_event(
            &mur_common::identity::AgentIdentity::generate(),
            "c1",
            &ChannelActor::System,
            EventKind::Message,
            &bad_payload,
            None,
        );
        let forged_ev = store
            .append_event(
                "c1",
                ChannelActor::System,
                EventKind::Message,
                bad_payload,
                None,
                Some(forged_sig),
                Some(0),
            )
            .unwrap();

        let (events, report) = store
            .load_events_with_damage("c1", &writer.verifying_key_bytes(), false)
            .unwrap();

        assert_eq!(
            events.len(),
            2,
            "the garbled line is skipped; the two parseable events are returned"
        );
        assert_eq!(report.unparseable_lines, vec![2], "1-based line number");
        assert_eq!(
            report.unverified.len(),
            1,
            "exactly the forged event is reported"
        );
        assert_eq!(report.unverified[0].seq, forged_ev.seq);
        assert!(!report.is_clean());

        // The good event is untouched and still present in `events`.
        assert!(events.iter().any(|e| e.seq == good_sig_ev.seq));
        assert!(events.iter().any(|e| e.seq == forged_ev.seq));
    }

    /// A clean, fully-signed log reports no damage at all.
    #[test]
    fn load_events_with_damage_is_clean_for_a_healthy_signed_log() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();
        let writer = mur_common::identity::AgentIdentity::generate();
        for n in 0..3 {
            let payload = serde_json::json!({"n": n});
            let sig = crate::sign::sign_event(
                &writer,
                "c1",
                &ChannelActor::System,
                EventKind::Message,
                &payload,
                None,
            );
            store
                .append_event(
                    "c1",
                    ChannelActor::System,
                    EventKind::Message,
                    payload,
                    None,
                    Some(sig),
                    Some(0),
                )
                .unwrap();
        }
        let (events, report) = store
            .load_events_with_damage("c1", &writer.verifying_key_bytes(), true)
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(report.is_clean());
    }

    /// An absent channel is genuine absence for the new path too — same
    /// `Ok((vec![], clean))` shape `load_events` returns `Ok(vec![])` for.
    #[test]
    fn load_events_with_damage_on_an_absent_channel_is_a_clean_empty_log() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        let id = mur_common::identity::AgentIdentity::generate();
        let (events, report) = store
            .load_events_with_damage("ghost", &id.verifying_key_bytes(), false)
            .unwrap();
        assert!(events.is_empty());
        assert!(report.is_clean());
    }

    /// The decision Windows depends on, tested directly. An end-to-end test
    /// cannot cover it on Unix: there the corrupted case fails with ENOTDIR
    /// and never reaches the `NotFound` arm at all, so it would pass with this
    /// logic deleted. Calling the function is the only way to exercise on this
    /// platform the branch Windows will actually take.
    #[test]
    fn check_absence_is_not_corruption_rejects_a_non_directory_only() {
        let tmp = TempDir::new().unwrap();

        // A path that is not there at all: genuine absence.
        check_absence_is_not_corruption(&tmp.path().join("never-created"))
            .expect("a missing channel dir is absence, not corruption");

        // A real directory: genuine absence of the log inside it.
        let dir = tmp.path().join("a-real-dir");
        std::fs::create_dir_all(&dir).unwrap();
        check_absence_is_not_corruption(&dir)
            .expect("an existing channel dir with no log is absence, not corruption");

        // A file where the directory should be: corruption.
        let file = tmp.path().join("a-file");
        std::fs::write(&file, b"not a directory").unwrap();
        let err = check_absence_is_not_corruption(&file)
            .expect_err("a channel path that is a FILE must not read as absence");
        assert!(
            format!("{err:#}").contains("a-file"),
            "the error must name the path: {err:#}"
        );
    }

    /// A channel directory replaced by a FILE is corruption, not absence — and
    /// it must read the same way on every platform. Unix reports ENOTDIR here
    /// and propagates; Windows maps the same condition to `NotFound`, which
    /// the absence arm would otherwise swallow into `Ok(vec![])`. On Unix this
    /// test passes through the ENOTDIR path, so it pins the end-to-end
    /// behaviour but does NOT prove the Windows branch — see the test above
    /// for that one.
    #[test]
    fn a_channel_path_that_is_not_a_directory_is_an_error_not_an_absence() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());
        store.create(&sample_channel("c1")).unwrap();

        let dir = tmp.path().join("channels").join("c1");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"i am a file, not a channel").unwrap();

        let err = store
            .load_events("c1")
            .expect_err("a channel path that is not a directory must not read as an empty log");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("c1"),
            "the error must name the channel it could not read: {msg}"
        );
    }

    /// The absence arm must still mean absence: a channel that was never
    /// created, and a created channel with no events yet, are both `Ok(vec![])`
    /// — the check above must not turn ordinary emptiness into a failure.
    #[test]
    fn genuine_absence_still_reads_as_an_empty_log() {
        let tmp = TempDir::new().unwrap();
        let store = ChannelStore::new(tmp.path());

        assert!(
            store.load_events("never-created").unwrap().is_empty(),
            "a channel that does not exist is empty, not an error"
        );

        store.create(&sample_channel("c1")).unwrap();
        assert!(
            store.load_events("c1").unwrap().is_empty(),
            "a created channel with no events yet is empty, not an error"
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
