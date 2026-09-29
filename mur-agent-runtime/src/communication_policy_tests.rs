//! Unit tests for caller identification and the accepts_from decision.
//! Process lineage is a fake tree so these are deterministic on every OS.

use super::*;

const SELF: u32 = 100;
const PEER: u32 = 200;
const CLI: u32 = 500;

/// 101 → 100 (self) ; 201 → 200 (peer "watcher") ; 501 → 500 → 1 (user shell)
fn tree(pid: u32) -> Option<u32> {
    match pid {
        101 => Some(SELF),
        202 => Some(201),
        201 => Some(PEER),
        PEER | SELF => Some(CLI),
        501 => Some(CLI),
        CLI => Some(1),
        _ => None,
    }
}

fn running() -> HashMap<u32, String> {
    HashMap::from([(PEER, "watcher".to_string())])
}

fn list(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_peer_and_its_descendants_are_that_peer() {
    for pid in [PEER, 201, 202] {
        assert_eq!(
            identify(Some(pid), SELF, &running(), tree),
            Caller::Agent("watcher".into()),
            "pid {pid}"
        );
    }
}

#[test]
fn own_children_are_self_even_when_a_peer_launched_us() {
    // Nearest match wins: SELF is reached before anything above it.
    assert_eq!(
        identify(Some(101), SELF, &running(), tree),
        Caller::SelfAgent
    );
    assert_eq!(
        identify(Some(SELF), SELF, &running(), tree),
        Caller::SelfAgent
    );
}

#[test]
fn a_process_outside_every_agent_is_the_user() {
    assert_eq!(identify(Some(501), SELF, &running(), tree), Caller::User);
    assert_eq!(identify(Some(CLI), SELF, &running(), tree), Caller::User);
}

#[test]
fn a_missing_or_zero_pid_is_unknown() {
    assert_eq!(identify(None, SELF, &running(), tree), Caller::Unknown);
    assert_eq!(identify(Some(0), SELF, &running(), tree), Caller::Unknown);
}

#[test]
fn a_parent_cycle_terminates() {
    let cyc = |p: u32| Some(if p == 7 { 8 } else { 7 });
    assert_eq!(identify(Some(7), SELF, &running(), cyc), Caller::User);
}

#[test]
fn restricted_list_refuses_unlisted_peer_but_never_the_user_or_self() {
    let l = list(&["notify_*"]);
    assert!(admits(&l, &Caller::Agent("watcher".into())).is_err());
    assert!(admits(&l, &Caller::Agent("notify_x".into())).is_ok());
    assert!(admits(&l, &Caller::User).is_ok());
    assert!(admits(&l, &Caller::SelfAgent).is_ok());
}

#[test]
fn unknown_caller_fails_closed_once_the_list_narrows() {
    assert!(admits(&list(&["*"]), &Caller::Unknown).is_ok());
    assert!(admits(&list(&["notify_*"]), &Caller::Unknown).is_err());
    assert!(admits(&[], &Caller::Unknown).is_err());
}

#[test]
fn an_empty_list_refuses_every_peer() {
    assert!(admits(&[], &Caller::Agent("watcher".into())).is_err());
    assert!(admits(&[], &Caller::User).is_ok());
}

#[test]
fn running_agents_skips_dead_pids_and_bad_entries() {
    let tmp = tempfile::TempDir::new().unwrap();
    let live = tmp.path().join("live");
    let dead = tmp.path().join("dead");
    let junk = tmp.path().join("junk");
    for d in [&live, &dead, &junk] {
        std::fs::create_dir_all(d).unwrap();
    }
    let lock = |name: &str, pid: u32| {
        serde_json::json!({
            "schema": 1, "uuid": "u", "name": name, "pid": pid, "ppid": 0,
            "started_at": "t", "binary_version": "v",
            "transports": {"stdio": false, "unix_socket": null, "tcp": null, "webhook": null},
            "card_digest": "d", "capabilities": []
        })
        .to_string()
    };
    std::fs::write(live.join("running.lock"), lock("live", std::process::id())).unwrap();
    // pid_max on every supported OS is far below this.
    std::fs::write(dead.join("running.lock"), lock("dead", 0x7fff_fff0)).unwrap();
    std::fs::write(junk.join("running.lock"), "not json").unwrap();

    let got = running_agents(tmp.path());
    assert_eq!(
        got.get(&std::process::id()).map(String::as_str),
        Some("live")
    );
    assert_eq!(got.len(), 1, "{got:?}");
}

#[test]
fn an_open_list_skips_identification() {
    let p = AcceptPolicy {
        accepts_from: list(&["*"]),
        agents_dir: PathBuf::from("/nonexistent"),
        self_pid: SELF,
    };
    assert!(p.check(None).is_ok());
}
