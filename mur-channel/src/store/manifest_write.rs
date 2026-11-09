//! The one locked manifest writer (N9). Every manifest change after `create`
//! goes through here: take the channel's append lock, re-read the manifest
//! under it, apply the edit, write temp + rename. A copy read before the lock
//! can never overwrite what another writer landed in between.
//!
//! The lock orders writers; it is not a tamper defence — `channels/` is
//! agent-writable.
use anyhow::Result;
use mur_common::channel::{CHANNEL_SCHEMA_VERSION, Channel};

use super::{ChannelStore, ManifestPolicy};

impl ChannelStore {
    /// Edit the manifest of `id` under its append lock and return the result.
    ///
    /// `v` is `max(on-disk v, v after f, CHANNEL_SCHEMA_VERSION)`: an edit can
    /// never lower it. A write that changes nothing is skipped. Not
    /// re-entrant — a caller already inside `with_append_lock` must use
    /// [`update_manifest_locked`](Self::update_manifest_locked).
    pub fn update_manifest(
        &self,
        id: &str,
        policy: ManifestPolicy,
        f: impl FnOnce(&mut Channel),
    ) -> Result<Channel> {
        self.with_append_lock(id, policy, || self.update_manifest_locked(id, f))
    }

    /// [`update_manifest`](Self::update_manifest) for a caller that ALREADY
    /// holds the append lock. Calling it without the lock forfeits the N9
    /// guarantee.
    pub fn update_manifest_locked(
        &self,
        id: &str,
        f: impl FnOnce(&mut Channel),
    ) -> Result<Channel> {
        let on_disk = self.load_manifest(id)?;
        let before = serde_yaml::to_string(&on_disk)?;
        let disk_v = on_disk.v;
        let mut ch = on_disk;
        f(&mut ch);
        ch.v = ch.v.max(disk_v).max(CHANNEL_SCHEMA_VERSION);
        if serde_yaml::to_string(&ch)? != before {
            self.write_manifest_file(&ch)?;
        }
        Ok(ch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::channel::{
        ChannelActor, ChannelPurpose, ChannelState, Goal, Participant, ParticipantRole,
    };
    use tempfile::TempDir;

    fn sample(id: &str) -> Channel {
        let now = chrono::Utc::now();
        Channel {
            v: CHANNEL_SCHEMA_VERSION,
            id: id.into(),
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

    fn store() -> (TempDir, ChannelStore) {
        let d = TempDir::new().unwrap();
        let s = ChannelStore::new(d.path());
        s.create(&sample("c1")).unwrap();
        (d, s)
    }

    #[test]
    fn update_manifest_never_lowers_v() {
        let (_d, s) = store();
        let ch = s
            .update_manifest("c1", ManifestPolicy::Require, |c| c.v = 0)
            .unwrap();
        assert_eq!(ch.v, CHANNEL_SCHEMA_VERSION);
        assert_eq!(s.load_manifest("c1").unwrap().v, CHANNEL_SCHEMA_VERSION);
    }

    #[test]
    fn concurrent_updates_do_not_lose_edits() {
        let (_d, s) = store();
        let s = std::sync::Arc::new(s);
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || {
                    s.update_manifest("c1", ManifestPolicy::Require, |c| {
                        c.participants.push(Participant {
                            actor: ChannelActor::Agent {
                                id: format!("a{i}"),
                            },
                            role: ParticipantRole::Delegate,
                            joined_at: chrono::Utc::now(),
                        })
                    })
                    .unwrap();
                })
            })
            .collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        assert_eq!(s.load_manifest("c1").unwrap().participants.len(), 8);
    }

    #[test]
    fn update_manifest_locked_inside_append_lock() {
        let (_d, s) = store();
        s.with_append_lock("c1", ManifestPolicy::Require, || {
            s.update_manifest_locked("c1", |c| c.purpose = Some(ChannelPurpose::Conversation))
                .map(|_| ())
        })
        .unwrap();
        assert!(s.load_manifest("c1").unwrap().purpose.is_some());
    }

    #[test]
    fn no_save_manifest_outside_create_and_tests() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = vec![];
        let mut stack = vec![root.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let name = p.file_name().unwrap().to_string_lossy().to_string();
                    if name.contains("test") {
                        continue;
                    }
                    let src = std::fs::read_to_string(&p).unwrap();
                    let prod = src.split("#[cfg(test)]").next().unwrap();
                    for (n, l) in prod.lines().enumerate() {
                        let t = l.trim_start();
                        if t.starts_with("//") {
                            continue;
                        }
                        if l.contains(".save_manifest(") {
                            hits.push(format!("{}:{}", p.display(), n + 1));
                        }
                    }
                }
            }
        }
        // `create` writes through `write_manifest_file`; nothing else in
        // production may call the unlocked `save_manifest`.
        assert!(hits.is_empty(), "unlocked save_manifest callers: {hits:?}");
    }
}
