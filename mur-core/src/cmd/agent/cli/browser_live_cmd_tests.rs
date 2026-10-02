use super::*;
use mur_common::agent::{McpNetMode, McpServerNetwork};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn live_entry() -> McpServerEntry {
    McpServerEntry {
        name: LIVE_ENTRY.into(),
        command: "mur".into(),
        ..Default::default()
    }
}

fn hosts_of(servers: &[McpServerEntry]) -> Vec<String> {
    let e = servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    let net = e.network.as_ref().expect("network set");
    assert_eq!(net.mode, McpNetMode::Restricted);
    net.allow_hosts.clone()
}

// 1. no host → refuse, with a runnable example
#[test]
fn no_host_is_refused_with_an_example() {
    let err = parse_hosts(&[]).unwrap_err();
    assert!(err.contains(USAGE_EXAMPLE), "{err}");
}

// 2. hosts → entry created, allowlist exact, restricted
#[test]
fn hosts_create_the_entry_with_that_allowlist() {
    let hosts = parse_hosts(&s(&["shop-a.example.com", "shop-b.example.com"])).unwrap();
    let mut servers = Vec::new();
    let out = apply(&mut servers, &hosts, live_entry);
    assert!(out.added);
    assert!(out.previous.is_empty());
    assert_eq!(hosts_of(&servers), hosts);
}

// 3. setup not done → refuse, naming the setup command
#[test]
fn missing_setup_is_refused_and_names_the_command() {
    let err = check_setup(false).unwrap_err();
    assert!(err.contains("mur browser setup"), "{err}");
    assert!(check_setup(true).is_ok());
}

// 4. same hosts twice → idempotent, entry not re-created
#[test]
fn repeat_with_same_hosts_is_idempotent() {
    let hosts = s(&["shop-a.example.com"]);
    let mut servers = Vec::new();
    apply(&mut servers, &hosts, live_entry);
    let out = apply(&mut servers, &hosts, || panic!("must not re-add"));
    assert!(!out.added);
    assert_eq!(servers.len(), 1);
    assert_eq!(hosts_of(&servers), hosts);
}

// 5. different hosts → overwrite, previous list reported
#[test]
fn repeat_with_other_hosts_overwrites_and_reports_old_list() {
    let mut servers = Vec::new();
    apply(&mut servers, &s(&["old.example.com"]), live_entry);
    let out = apply(&mut servers, &s(&["new.example.com"]), || {
        panic!("must not re-add")
    });
    assert_eq!(out.previous, s(&["old.example.com"]));
    assert_eq!(hosts_of(&servers), s(&["new.example.com"]));
}

// 5b. an existing entry with a foreign network mode is still overwritten
#[test]
fn existing_entry_network_is_replaced_not_merged() {
    let mut e = live_entry();
    e.network = Some(McpServerNetwork {
        allow_hosts: s(&["x.example.com"]),
        ..Default::default()
    });
    let mut servers = vec![e];
    let out = apply(&mut servers, &s(&["y.example.com"]), || {
        panic!("must not re-add")
    });
    assert_eq!(out.previous, s(&["x.example.com"]));
    assert_eq!(hosts_of(&servers), s(&["y.example.com"]));
}

// 6. --add routing unchanged; live and turns route as designed
#[test]
fn routing_keeps_add_and_splits_live() {
    assert_eq!(route(&s(&["--add"])), Route::Add);
    assert_eq!(
        route(&s(&["live", "a.example.com"])),
        Route::Live(s(&["a.example.com"]))
    );
    assert_eq!(route(&s(&["live"])), Route::Live(vec![]));
    assert_eq!(route(&s(&["check", "the", "cart"])), Route::Turn);
    assert_eq!(route(&[]), Route::Turn);
}

// 7. non-web port → refused (before any write), pointing at allow-port
#[test]
fn non_web_port_is_refused_and_points_at_allow_port() {
    let err = parse_hosts(&s(&["shop-a.example.com:9000"])).unwrap_err();
    assert!(err.contains("perm allow-port"), "{err}");
    assert!(err.contains("9000"), "{err}");
}

// 8. web-set ports are accepted (guards the RESTRICTED_GENERAL_PORTS contract)
#[test]
fn web_set_ports_are_accepted() {
    for p in mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS {
        let h = format!("shop-a.example.com:{p}");
        assert!(parse_hosts(std::slice::from_ref(&h)).is_ok(), "{h}");
    }
    assert!(parse_hosts(&s(&["a.example.com:443", "b.example.com:8080"])).is_ok());
}

// 9. ports are stripped and duplicates collapse
#[test]
fn port_is_normalized_away_and_duplicates_collapse() {
    let hosts = parse_hosts(&s(&["shop-a.example.com:443", "shop-a.example.com"])).unwrap();
    assert_eq!(hosts, s(&["shop-a.example.com"]));
}

/// Fake [`LiveOps`]: records every call, never touches disk or spawns.
#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    adds: Vec<Vec<String>>,
    saves: usize,
    fail_save: bool,
    removes: usize,
    fail_remove: bool,
}

impl LiveOps for Fake {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(self.servers.clone())
    }
    fn mcp_add(&mut self, argv: &[String]) -> anyhow::Result<String> {
        self.adds.push(argv.to_vec());
        let mut e = live_entry();
        e.args = argv.to_vec();
        self.servers.push(e);
        Ok("added".into())
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()> {
        self.saves += 1;
        if self.fail_save {
            anyhow::bail!("disk full");
        }
        self.servers = servers;
        Ok(())
    }
    fn remove(&mut self, name: &str) -> anyhow::Result<()> {
        self.removes += 1;
        if self.fail_remove {
            anyhow::bail!("profile locked");
        }
        self.servers.retain(|e| e.name != name);
        Ok(())
    }
}

// 7 (flow): refused port → no add, no save, and no chip (it's an Err)
#[test]
fn run_refusal_writes_nothing_and_offers_no_chip() {
    let mut f = Fake::default();
    let err = run("bob", &s(&["shop-a.example.com:9000"]), true, &mut f).unwrap_err();
    assert!(err.to_string().contains("perm allow-port"), "{err}");
    assert!(f.adds.is_empty() && f.saves == 0);
    let mut f = Fake::default();
    assert!(run("bob", &s(&["a.example.com"]), false, &mut f).is_err());
    assert!(f.adds.is_empty() && f.saves == 0);
}

// 2/4/5 (flow): add once with live-<agent> argv, chip offered, idempotent
#[test]
fn run_adds_once_with_per_agent_run_and_offers_restart() {
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["old.example.com"]), true, &mut f).unwrap();
    assert_eq!(
        f.adds,
        vec![s(&[
            "browser", "record", "--run", "live-bob", "--mode", "live"
        ])]
    );
    assert!(chip.is_some_and(|p| p.is_executable()), "{text}");
    let (text, _) = run("bob", &s(&["new.example.com"]), true, &mut f).unwrap();
    assert_eq!(f.adds.len(), 1, "second call must not re-add");
    assert!(text.contains("old.example.com"), "{text}");
    assert_eq!(hosts_of(&f.servers), s(&["new.example.com"]));
}

// 11a: entry created this call + allowlist save fails → rolled back, Err
#[test]
fn save_failure_rolls_back_an_entry_created_here() {
    let mut f = Fake {
        fail_save: true,
        ..Fake::default()
    };
    let err = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap_err();
    assert_eq!(f.adds.len(), 1);
    assert_eq!(f.removes, 1);
    assert!(
        f.servers.iter().all(|e| e.name != LIVE_ENTRY),
        "entry left behind"
    );
    assert!(format!("{err:#}").contains("disk full"), "{err:#}");
}

// 11b: entry already existed + save fails → leave it alone, Err
#[test]
fn save_failure_keeps_a_preexisting_entry() {
    let mut f = Fake {
        servers: vec![live_entry()],
        fail_save: true,
        ..Fake::default()
    };
    assert!(run("bob", &s(&["a.example.com"]), true, &mut f).is_err());
    assert_eq!(f.removes, 0);
    assert!(f.servers.iter().any(|e| e.name == LIVE_ENTRY));
}

// 11c: rollback itself fails → both errors surface, plus a manual-check hint
#[test]
fn failed_rollback_reports_both_errors() {
    let mut f = Fake {
        fail_save: true,
        fail_remove: true,
        ..Fake::default()
    };
    let msg = format!(
        "{:#}",
        run("bob", &s(&["a.example.com"]), true, &mut f).unwrap_err()
    );
    for want in ["disk full", "profile locked", "check the profile"] {
        assert!(msg.contains(want), "missing {want:?} in {msg}");
    }
}
