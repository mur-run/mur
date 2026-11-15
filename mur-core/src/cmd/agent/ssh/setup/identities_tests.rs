//! The install connection offers the user's keys only (plan K7).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use mur_ssh_request::RequestHost;

use super::identities::{
    AgentListing, DEFAULT_IDENTITY_FILES, Identities, NO_OWN_KEY, dedicated_blobs,
    default_identity_files, identity_args, own_agent_keys,
};
use super::install::{InstallRequest, install_argv};

const USER_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHVzZXJ1c2VydXNlcnVzZXJ1c2VydXNlcnVzZXI";
const MUR_BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIG11cm11cm11cm11cm11cm11cm11cm11cm11cjE";
const LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f mur-agent-bot";

fn strs(argv: &[std::ffi::OsString]) -> Vec<String> {
    argv.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
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
fn only_default_identity_files_that_exist_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("id_ed25519"), "k").unwrap();
    fs::write(dir.path().join("config"), "Host *").unwrap();
    fs::create_dir(dir.path().join("id_rsa")).unwrap();
    assert!(DEFAULT_IDENTITY_FILES.contains(&"id_rsa"));
    assert_eq!(
        default_identity_files(dir.path()),
        vec![dir.path().join("id_ed25519")]
    );
}

#[test]
fn nothing_to_offer_is_refused_before_any_connection() {
    let dir = tempfile::tempdir().unwrap();
    let err = Identities::prepare(Some(dir.path()), &[], vec![])
        .err()
        .unwrap();
    assert_eq!(err.to_string(), NO_OWN_KEY);
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "no file written"
    );

    let host = RequestHost::parse("gh.example", None).unwrap();
    let r = InstallRequest {
        host: &host,
        user: "git",
        public_line: LINE,
        known_hosts: "gh.example ssh-ed25519 AAAAH\n",
    };
    assert!(install_argv(&r, Path::new("/k/known"), &[]).is_err());
}

#[test]
fn agent_keys_become_private_pub_files_gone_after_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let user = format!("ssh-ed25519 {USER_BLOB} david@laptop");
    let disk = PathBuf::from("/u/.ssh/id_rsa");
    let ids = Identities::prepare(
        Some(dir.path()),
        std::slice::from_ref(&user),
        vec![disk.clone()],
    )
    .unwrap();
    let files = ids.files().to_vec();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1], disk);
    let tmp = &files[0];
    assert_eq!(tmp.parent(), Some(dir.path()));
    assert!(
        tmp.to_string_lossy().ends_with(".pub"),
        "ssh signs a .pub through the agent"
    );
    assert_eq!(fs::read_to_string(tmp).unwrap(), format!("{user}\n"));
    drop(ids);
    assert!(!tmp.exists());
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
    let argv = strs(&install_argv(&r, Path::new("/k/known"), &ids).unwrap());
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
