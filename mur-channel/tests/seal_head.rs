//! The real sealing hook (channel-log-integrity spec 7.3.1 and section 9).

use std::fs;
use std::sync::Arc;

use chrono::Utc;
use mur_channel::ChannelStore;
use mur_channel::head::Head;
use mur_channel::head_io::read_head;
use mur_channel::seal::{HEADS_DIR_NAME, HeadSealer};
use mur_common::channel::{Channel, ChannelActor, ChannelState, EventKind, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

const CH: &str = "head-ch";
const KV: u32 = 3;

fn human() -> ChannelActor {
    ChannelActor::Human { name: "me".into() }
}

struct Fx {
    tmp: TempDir,
    store: ChannelStore,
    id: Arc<AgentIdentity>,
}

fn fx() -> Fx {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let now = Utc::now();
    store
        .create(&Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: CH.into(),
            title: "t".into(),
            goal: Goal::default(),
            state: ChannelState::Working,
            purpose: None,
            owner: human(),
            participants: vec![],
            created_at: now,
            updated_at: now,
        })
        .unwrap();
    Fx {
        tmp,
        store,
        id: Arc::new(AgentIdentity::generate()),
    }
}

impl Fx {
    fn sealer(&self) -> HeadSealer {
        HeadSealer::new(self.tmp.path(), self.id.clone(), KV)
    }
    fn heads(&self) -> std::path::PathBuf {
        self.tmp.path().join(HEADS_DIR_NAME)
    }
    fn head(&self) -> Option<Head> {
        read_head(&self.heads(), CH).unwrap()
    }
    fn plain(&self, text: &str) {
        self.append(text, None, None).unwrap();
    }
    fn append(
        &self,
        text: &str,
        key: Option<&str>,
        seal: Option<&mur_channel::store::SealHook<'_>>,
    ) -> anyhow::Result<mur_common::channel::ChannelEvent> {
        self.store.append_event_with(
            CH,
            human(),
            EventKind::Message,
            serde_json::json!({ "text": text }),
            key.map(str::to_string),
            None,
            None,
            seal,
        )
    }
    fn sealed(&self, text: &str) -> anyhow::Result<mur_common::channel::ChannelEvent> {
        let s = self.sealer();
        let hook = move |i: &mur_channel::store::SealInput<'_>| s.seal(i);
        self.append(text, None, Some(&hook))
    }
}

#[test]
fn channel_born_with_a_head_has_null_legacy_through() {
    let f = fx();
    let ev = f.sealed("first").unwrap();
    let h = f.head().expect("head written");
    assert_eq!(h.seq, ev.seq);
    assert_eq!(h.lines, 1);
    assert_eq!(h.legacy_through, None);
    assert_eq!(h.key_version, Some(KV));
    assert!(h.verify(&f.id.verifying_key_bytes()));
}

#[test]
fn first_seal_of_a_legacy_log_records_the_last_prior_seq_and_keeps_it() {
    let f = fx();
    f.plain("legacy 0");
    f.plain("legacy 1");
    let ev = f.sealed("router").unwrap();
    let h = f.head().unwrap();
    assert_eq!(h.legacy_through, Some(1));
    assert_eq!(h.seq, ev.seq);
    assert_eq!(h.lines, 3);

    f.sealed("router 2").unwrap();
    let h2 = f.head().unwrap();
    assert_eq!(
        h2.legacy_through,
        Some(1),
        "carried forward, never recomputed"
    );
    assert_eq!(h2.lines, 4);
}

#[test]
fn a_later_seal_covers_unsigned_lines_written_in_between() {
    let f = fx();
    f.sealed("r0").unwrap();
    f.plain("hub line");
    let ev = f.sealed("r2").unwrap();
    let h = f.head().unwrap();
    assert_eq!(h.lines, 3);
    assert_eq!(h.seq, ev.seq);
}

#[test]
fn dedup_retry_does_not_move_a_head_that_already_covers_it() {
    let f = fx();
    let s = f.sealer();
    let hook = move |i: &mur_channel::store::SealInput<'_>| s.seal(i);
    f.append("a", Some("k1"), Some(&hook)).unwrap();
    let before = f.head().unwrap();
    f.append("a", Some("k1"), Some(&hook)).unwrap();
    assert_eq!(f.head().unwrap(), before);
}

#[test]
fn dedup_retry_seals_an_event_whose_first_seal_failed() {
    let f = fx();
    let fail = |_: &mur_channel::store::SealInput<'_>| -> anyhow::Result<()> { anyhow::bail!("x") };
    assert!(f.append("a", Some("k1"), Some(&fail)).is_err());
    assert!(f.head().is_none());
    let s = f.sealer();
    let hook = move |i: &mur_channel::store::SealInput<'_>| s.seal(i);
    f.append("a", Some("k1"), Some(&hook)).unwrap();
    let h = f.head().unwrap();
    assert_eq!(h.lines, 1);
    assert_eq!(
        h.legacy_through, None,
        "the retried line is the router's own"
    );
}

#[test]
fn first_seal_with_only_unparseable_lines_before_it_fails_and_writes_no_head() {
    let f = fx();
    let log = f.store.events_path(CH);
    fs::write(&log, b"not json\n").unwrap();
    let err = f.sealed("router").unwrap_err();
    assert!(format!("{err:#}").contains("legacy_through"), "{err:#}");
    assert!(f.head().is_none());
    assert_eq!(
        fs::read_to_string(&log).unwrap().lines().count(),
        2,
        "line stays"
    );
}

#[test]
fn a_truncated_log_is_not_sealed_over() {
    let f = fx();
    f.sealed("r0").unwrap();
    f.sealed("r1").unwrap();
    let before = f.head().unwrap();
    let log = f.store.events_path(CH);
    let text = fs::read_to_string(&log).unwrap();
    let first = text.lines().next().unwrap();
    fs::write(&log, format!("{first}\n")).unwrap();
    assert!(f.sealed("r2").is_err());
    assert_eq!(f.head().unwrap(), before, "evidence head untouched");
}

#[test]
fn an_edited_line_is_not_sealed_over() {
    let f = fx();
    f.sealed("r0").unwrap();
    let before = f.head().unwrap();
    let log = f.store.events_path(CH);
    let text = fs::read_to_string(&log).unwrap().replace("r0", "rX");
    fs::write(&log, text).unwrap();
    assert!(f.sealed("r1").is_err());
    assert_eq!(f.head().unwrap(), before);
}

#[test]
fn a_head_signed_by_another_key_is_not_replaced() {
    let f = fx();
    f.sealed("r0").unwrap();
    let before = f.head().unwrap();
    let other = HeadSealer::new(f.tmp.path(), Arc::new(AgentIdentity::generate()), KV);
    let hook = move |i: &mur_channel::store::SealInput<'_>| other.seal(i);
    assert!(f.append("r1", None, Some(&hook)).is_err());
    assert_eq!(f.head().unwrap(), before);
}

#[test]
fn hook_is_usable_through_the_service() {
    let f = fx();
    let svc = mur_channel::ChannelService::open(f.tmp.path())
        .unwrap()
        .with_seal(f.sealer().hook());
    drop(svc);
}
