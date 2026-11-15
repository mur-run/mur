//! The install connection offers the user's keys only (plan K7).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use mur_ssh_request::RequestHost;

use std::time::Duration;

use super::identities::{
    AgentListing, Identities, Offer, RUN_DIR_PREFIX, RUN_DIR_STALE_AFTER, dedicated_blobs,
    identity_args, list_agent, own_agent_keys, sweep_stale,
};
use super::install::{InstallRequest, install_argv};
use super::ssh_config::{AgentSocket, Expand, Resolved};

const USER_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHVzZXJ1c2VydXNlcnVzZXJ1c2VydXNlcnVzZXI";
const MUR_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIG11cm11cm11cm11cm11cm11cm11cm11cm11cjE";
const LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f mur-agent-bot";

fn strs(argv: &[std::ffi::OsString]) -> Vec<String> {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

fn expand_in(home: &Path) -> Expand<'_> {
    Expand {
        home,
        local_user: Some("david"),
        host: "gh.example",
        remote_user: "git",
        port: 22,
    }
}

fn listing(lines: &[String]) -> AgentListing {
    AgentListing::Keys(lines.join("\n") + "\n")
}

#[test]
fn a_dedicated_key_is_dropped_by_blob_and_by_comment() {
    let user = format!("ssh-ed25519 {USER_BLOB} david@laptop");
    let by_blob = format!("ssh-ed25519 {MUR_BLOB} renamed-by-some-agent");
    let by_comment =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIG90aGVyb3RoZXJvdGhlcm90aGVyb3RoZXI mur-agent-ops"
            .to_string();
    let dedicated: HashSet<String> = [MUR_BLOB.to_string()].into();

    let kept = own_agent_keys(&listing(&[user.clone(), by_blob, by_comment]), &dedicated);
    assert_eq!(kept, vec![user]);
}

#[test]
fn an_empty_or_missing_agent_keeps_nothing() {
    let none = HashSet::new();
    assert!(own_agent_keys(&AgentListing::Empty, &none).is_empty());
    assert!(own_agent_keys(&AgentListing::Unavailable, &none).is_empty());
    assert!(own_agent_keys(&AgentListing::Keys("\n  \n".into()), &none).is_empty());
}

#[test]
fn a_disabled_agent_is_not_asked() {
    assert_eq!(
        list_agent(&AgentSocket::Disabled),
        AgentListing::Unavailable
    );
}

#[test]
fn nothing_to_offer_writes_nothing_and_falls_back_to_ssh() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        Identities::prepare(Some(dir.path()), &[], &[])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "no file written"
    );

    // No `ssh -G` answer: before-K7 connection, no IdentitiesOnly.
    let home = tempfile::tempdir().unwrap();
    let x = expand_in(home.path());
    let offer = Offer::choose(Some(dir.path()), None, &x, home.path()).unwrap();
    assert!(offer.files().is_none());

    let host = RequestHost::parse("gh.example", None).unwrap();
    let r = InstallRequest {
        host: &host,
        user: "git",
        public_line: LINE,
        known_hosts: "gh.example ssh-ed25519 AAAAH\n",
    };
    for files in [None, Some(&[][..])] {
        let argv = strs(&install_argv(&r, Path::new("/k/known"), files).unwrap());
        assert!(!argv.iter().any(|a| a == "IdentitiesOnly=yes"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "-i"));
    }
}

#[test]
fn resolved_but_empty_falls_back_instead_of_refusing() {
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let x = expand_in(home.path());
    let r = Resolved {
        agent: AgentSocket::Disabled,
        files: vec!["~/.ssh/id_ed25519".into()],
    };
    let offer = Offer::choose(Some(dir.path()), Some(&r), &x, home.path()).unwrap();
    assert!(
        offer.files().is_none(),
        "no file exists, no agent: ssh decides"
    );
}

#[test]
fn config_files_come_first_then_agent_keys_in_a_private_run_dir() {
    let dir = tempfile::tempdir().unwrap();
    let user = format!("ssh-ed25519 {USER_BLOB} david@laptop");
    let work = PathBuf::from("/u/.ssh/work_key");
    let ids = Identities::prepare(
        Some(dir.path()),
        std::slice::from_ref(&work),
        std::slice::from_ref(&user),
    )
    .unwrap()
    .unwrap();
    let files = ids.files().to_vec();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0], work, "the per-host IdentityFile is offered first");
    let tmp = &files[1];
    let run_dir = tmp.parent().unwrap().to_path_buf();
    assert_eq!(run_dir.parent(), Some(dir.path()));
    assert!(
        run_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(RUN_DIR_PREFIX)
    );
    assert!(
        tmp.to_string_lossy().ends_with(".pub"),
        "ssh signs a .pub through the agent"
    );
    assert_eq!(fs::read_to_string(tmp).unwrap(), format!("{user}\n"));
    drop(ids);
    assert!(!run_dir.exists(), "the whole run dir goes with the run");
}

#[test]
fn a_run_dir_left_by_a_dead_run_is_swept() {
    let dir = tempfile::tempdir().unwrap();
    let stale = dir.path().join(format!("{RUN_DIR_PREFIX}dead"));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("agent-0.pub"), "k").unwrap();
    let other = dir.path().join("id_ed25519");
    fs::write(&other, "key").unwrap();

    sweep_stale(dir.path(), RUN_DIR_STALE_AFTER);
    assert!(
        stale.exists(),
        "a fresh one may belong to a run in progress"
    );
    sweep_stale(dir.path(), Duration::ZERO);
    assert!(!stale.exists());
    assert!(other.exists(), "only run dirs are touched");
}

#[test]
fn the_argv_carries_identities_only_once_and_one_dash_i_per_key() {
    let host = RequestHost::parse("gh.example", None).unwrap();
    let r = InstallRequest {
        host: &host,
        user: "git",
        public_line: LINE,
        known_hosts: "gh.example ssh-ed25519 AAAAH\n",
    };
    let ids = [
        PathBuf::from("/s/a b.pub"),
        PathBuf::from("/u/.ssh/id_ed25519"),
    ];
    let argv = strs(&install_argv(&r, Path::new("/k/known"), Some(&ids)).unwrap());
    assert_eq!(
        argv.iter().filter(|a| *a == "IdentitiesOnly=yes").count(),
        1
    );
    let dash_i: Vec<&String> = argv
        .iter()
        .enumerate()
        .filter(|(i, _)| *i > 0 && argv[i - 1] == "-i")
        .map(|(_, a)| a)
        .collect();
    assert_eq!(dash_i, vec!["/s/a b.pub", "/u/.ssh/id_ed25519"]);
    assert!(
        !argv.iter().any(|a| a == "-F"),
        "the user's ssh config is still read"
    );
    let sep = argv.iter().position(|a| a == "--").unwrap();
    assert!(argv.iter().rposition(|a| a == "-i").unwrap() < sep);
    assert_eq!(identity_args(&ids).len(), 2 + 2 * ids.len());
}

#[test]
fn every_stored_dedicated_key_is_known_by_blob() {
    let home = tempfile::tempdir().unwrap();
    assert!(dedicated_blobs(home.path()).is_empty(), "no key store yet");
    let a = super::dedicated::generate(home.path(), "alpha").unwrap();
    let b = super::dedicated::generate(home.path(), "beta").unwrap();
    let blobs = dedicated_blobs(home.path());
    for k in [a, b] {
        let blob = k.public_line.split_whitespace().nth(1).unwrap().to_string();
        assert!(blobs.contains(&blob));
    }
    assert_eq!(blobs.len(), 2);
}

#[test]
fn collecting_blobs_never_rebuilds_another_agents_pub() {
    let home = tempfile::tempdir().unwrap();
    let k = super::dedicated::generate(home.path(), "alpha").unwrap();
    let public = {
        let mut s = k.private.clone().into_os_string();
        s.push(".pub");
        PathBuf::from(s)
    };
    fs::remove_file(&public).unwrap();
    assert!(dedicated_blobs(home.path()).is_empty());
    assert!(
        !public.exists(),
        "read only: nothing written under secrets/"
    );
}
