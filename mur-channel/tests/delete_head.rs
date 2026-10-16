//! `delete_channel` removes the channel's head, after the directory (spec 7.4,
//! #1770). The order matters: a crash between the two leaves an orphan head
//! (`LogMissing`, benign), never a log with no head (`NoHead`).

use std::fs;
use std::path::PathBuf;

use mur_channel::ChannelService;
use mur_channel::anchor::{self, heads_dir};
use mur_channel::head_io::{head_path, read_head};
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn sealed_service(tmp: &TempDir) -> ChannelService {
    ChannelService::open(tmp.path()).unwrap()
}

/// A channel with one anchored append, so it has a head in `channel-heads/`.
fn sealed_channel(svc: &ChannelService, tmp: &TempDir) -> String {
    let id = svc.create_for_agent("del-agent").unwrap().id;
    svc.append_and_anchor(
        &id,
        &AgentIdentity::generate(),
        1,
        tmp.path(),
        ChannelActor::Human { name: "me".into() },
        EventKind::Message,
        serde_json::json!({ "text": "hi" }),
        None,
    )
    .unwrap();
    id
}

#[test]
fn delete_removes_the_directory_the_index_row_and_the_head() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let id = sealed_channel(&svc, &tmp);
    let heads = heads_dir(tmp.path());
    assert!(
        read_head(&heads, &id).unwrap().is_some(),
        "precondition: the append sealed a head"
    );

    svc.delete_channel(&id).unwrap();

    assert!(!tmp.path().join("channels").join(&id).exists());
    assert!(read_head(&heads, &id).unwrap().is_none());
    assert!(!head_path(&heads, &id).unwrap().exists());
}

#[test]
fn delete_leaves_other_channels_heads_alone() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let a = sealed_channel(&svc, &tmp);
    let b = sealed_channel(&svc, &tmp);
    let heads = heads_dir(tmp.path());

    svc.delete_channel(&a).unwrap();

    assert!(read_head(&heads, &a).unwrap().is_none());
    assert!(read_head(&heads, &b).unwrap().is_some());
}

#[test]
fn delete_clears_a_temp_a_crashed_write_left_behind() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let id = sealed_channel(&svc, &tmp);
    let head = head_path(&heads_dir(tmp.path()), &id).unwrap();
    let mut tmp_name = head.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let stale = PathBuf::from(tmp_name);
    fs::write(&stale, b"half a head").unwrap();

    svc.delete_channel(&id).unwrap();

    assert!(!head.exists());
    assert!(!stale.exists(), "a stale temp must not outlive the channel");
}

#[test]
fn delete_clears_the_dirty_marker_an_interrupted_append_left() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let id = sealed_channel(&svc, &tmp);
    // An anchored append that died between `mark_dirty` and `clear_dirty`.
    anchor::mark_dirty(tmp.path(), &id).unwrap();
    assert!(anchor::is_dirty(tmp.path(), &id));

    svc.delete_channel(&id).unwrap();

    assert!(
        !anchor::is_dirty(tmp.path(), &id),
        "a dirty marker must not outlive its channel"
    );
    assert!(!head_path(&heads_dir(tmp.path()), &id).unwrap().exists());
}

#[test]
fn deleting_a_never_sealed_channel_succeeds_without_a_heads_dir() {
    let tmp = TempDir::new().unwrap();
    // No anchored append, so no head and no `channel-heads/` directory.
    let svc = ChannelService::open(tmp.path()).unwrap();
    let id = svc.create_for_agent("plain-agent").unwrap().id;
    assert!(!heads_dir(tmp.path()).exists());

    svc.delete_channel(&id).unwrap();

    assert!(!heads_dir(tmp.path()).exists());
}

#[test]
fn deleting_twice_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let id = sealed_channel(&svc, &tmp);
    svc.delete_channel(&id).unwrap();
    svc.delete_channel(&id).unwrap();
}

#[test]
fn a_hostile_id_is_refused_before_the_store_is_touched() {
    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let victim = sealed_channel(&svc, &tmp);
    // `..` would make `channels/..` the directory to delete.
    assert!(svc.delete_channel("..").is_err());
    assert!(svc.delete_channel("a/b").is_err());
    assert!(
        tmp.path().join("channels").join(&victim).exists(),
        "a refused delete must not remove anything"
    );
    assert!(
        read_head(&heads_dir(tmp.path()), &victim)
            .unwrap()
            .is_some()
    );
}

#[cfg(unix)]
#[test]
fn a_failed_head_removal_leaves_an_orphan_head_not_a_headless_log() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let svc = sealed_service(&tmp);
    let id = sealed_channel(&svc, &tmp);
    let heads = heads_dir(tmp.path());
    // A read-only heads dir makes the unlink fail after the directory is gone.
    fs::set_permissions(&heads, fs::Permissions::from_mode(0o500)).unwrap();
    let writable = fs::metadata(&heads).is_ok() && fs::write(heads.join(".probe"), b"x").is_ok();
    if writable {
        // Running as root: permissions do not bind, the scenario cannot be set up.
        fs::remove_file(heads.join(".probe")).unwrap();
        fs::set_permissions(&heads, fs::Permissions::from_mode(0o700)).unwrap();
        return;
    }

    let res = svc.delete_channel(&id);
    fs::set_permissions(&heads, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(res.is_err(), "the failed head removal must surface");
    assert!(
        !tmp.path().join("channels").join(&id).exists(),
        "the directory goes first"
    );
    assert!(
        read_head(&heads, &id).unwrap().is_some(),
        "orphan head left behind: LogMissing, visible and benign"
    );
}
