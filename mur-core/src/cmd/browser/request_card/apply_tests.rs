use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_browser_request::{BrowserRequest, PendingRequest, Site, read_pending};
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;
use crate::cmd::browser::live_effect::Effect;
use crate::cmd::browser::live_entry::attached_profiles;
use crate::cmd::browser::request_card::Offer;

fn live(allow: &[&str], bound: &[&str], mode: McpNetMode) -> McpServerEntry {
    let mut args = vec!["browser".to_string(), "live".to_string()];
    for p in bound {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let mut e: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": args }),
    )
    .unwrap();
    e.network = Some(McpServerNetwork {
        mode,
        allow_hosts: allow.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    });
    e
}

#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    saves: usize,
    bound: Vec<(Option<String>, String)>,
    /// Another writer changes the entries between the check and the save.
    race: bool,
}

impl LiveOps for Fake {
    fn servers(&mut self) -> Result<Vec<McpServerEntry>> {
        Ok(self.servers.clone())
    }
    fn mcp_add(&mut self, _: &[String], _: McpServerNetwork) -> Result<String> {
        panic!("the card never creates an entry")
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> Result<(Vec<String>, Effect)> {
        self.saves += 1;
        self.servers = servers;
        Ok((Vec::new(), Effect::NextTurn))
    }
    fn save_if_unchanged(
        &mut self,
        expected: &[McpServerEntry],
        servers: Vec<McpServerEntry>,
    ) -> Result<Option<(Vec<String>, Effect)>> {
        if self.race || self.servers != expected {
            return Ok(None);
        }
        self.save(servers).map(Some)
    }
    fn pending_repairs(&mut self, _: &[McpServerEntry]) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn record_research(&mut self, _: &str) -> Result<()> {
        panic!("the card never enables research mode")
    }
    fn record_profile_bound(&mut self, _: &str, old: Option<&str>, new: &str) -> Result<()> {
        self.bound.push((old.map(Into::into), new.into()));
        Ok(())
    }
}

#[derive(Default)]
struct Ports {
    opened: Vec<u16>,
    fail: bool,
}
impl PortOps for Ports {
    fn open(&mut self, port: u16) -> Result<String> {
        anyhow::ensure!(!self.fail, "profile.yaml is read-only");
        self.opened.push(port);
        Ok(format!("granted outbound TCP port {port}"))
    }
}

fn request(home: &Path, site: &str, login: bool) -> PendingRequest {
    let s = Site::parse(site).unwrap();
    mur_browser_request::write(
        home,
        &BrowserRequest::new(&s, login, "review a PR", Utc::now()).unwrap(),
    )
    .unwrap();
    read_pending(home, Utc::now()).remove(0)
}

fn left(home: &Path) -> usize {
    read_pending(home, Utc::now()).len()
}

fn disjoint(_: &str) -> Option<Vec<String>> {
    Some(Vec::new())
}

fn hosts(h: &[&str]) -> Vec<String> {
    h.iter().map(|s| s.to_string()).collect()
}

fn use_(profile: &str, replace: bool, approved: &[&str]) -> Choice {
    Choice::Use {
        profile: profile.into(),
        domains: hosts(&["gitea.example"]),
        replace,
        approved: hosts(approved),
    }
}

struct Run<'a> {
    open_port: bool,
    login_saved: Option<&'a str>,
    bound: &'a BoundDomains<'a>,
    /// The entries the card showed; `None` means what `Fake` holds now.
    snapshot: Option<Vec<McpServerEntry>>,
}

impl Default for Run<'_> {
    fn default() -> Self {
        Run {
            open_port: false,
            login_saved: None,
            bound: &disjoint,
            snapshot: None,
        }
    }
}

fn run(
    f: &mut Fake,
    ports: &mut Ports,
    home: &Path,
    r: &PendingRequest,
    c: &Choice,
    o: Run<'_>,
) -> Answered {
    let snapshot = o.snapshot.unwrap_or_else(|| f.servers.clone());
    let a = Answer {
        agent: "mur",
        agent_home: home,
        request: r,
        port: r.site.port(),
        snapshot: &snapshot,
        login_saved: o.login_saved,
    };
    answer(&a, c, o.open_port, o.bound, f, ports).unwrap()
}

fn profiles(f: &Fake) -> Vec<String> {
    attached_profiles(&f.servers[0])
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn an_offered_profile_is_bound_the_request_cleared_and_handed_back() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    let Answered::Done {
        hand_back: Some(h),
        effect,
        ..
    } = got
    else {
        panic!("{got:?}")
    };
    assert_eq!(profiles(&f), vec!["g"]);
    assert_eq!(f.bound, vec![(None, "g".to_string())]);
    assert_eq!(effect, Some(Effect::NextTurn));
    assert!(
        h.contains("https://gitea.example") && h.contains("agent-reported"),
        "{h}"
    );
    assert_eq!(left(t.path()), 0);
}

#[test]
fn adds_hosts_writes_the_approved_host_in_the_same_save() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(&["other.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &["gitea.example"]),
        Run::default(),
    );
    assert_eq!(f.saves, 1);
    let net = f.servers[0].network.as_ref().unwrap();
    assert_eq!(net.allow_hosts, vec!["other.example", "gitea.example"]);
    assert_eq!(profiles(&f), vec!["g"]);
}

/// Council B1 + D11: a subdomain request needs its own host; the answer
/// adds it only when the card named it.
#[test]
fn a_subdomain_request_without_approval_is_stale() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "chat.deepseek.com", true);
    let ds = |approved: &[&str]| Choice::Use {
        profile: "ds".into(),
        domains: hosts(&["deepseek.com"]),
        replace: false,
        approved: hosts(approved),
    };
    let mut f = Fake {
        servers: vec![live(&["deepseek.com"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &ds(&[]),
        Run::default(),
    );
    assert_eq!((got, f.saves, left(t.path())), (Answered::Stale, 0, 1));

    run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &ds(&["chat.deepseek.com"]),
        Run::default(),
    );
    let net = f.servers[0].network.as_ref().unwrap();
    assert_eq!(net.allow_hosts, vec!["deepseek.com", "chat.deepseek.com"]);
    assert_eq!(left(t.path()), 0);
}

#[test]
fn an_overlap_needs_replace_and_then_names_what_it_dropped() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let bound = |n: &str| (n == "old").then(|| vec!["gitea.example".to_string()]);
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &["old"], McpNetMode::Restricted)],
        ..Default::default()
    };

    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run {
            bound: &bound,
            ..Default::default()
        },
    );
    assert!(
        matches!(got, Answered::Refused(ref w) if w.contains("old")),
        "{got:?}"
    );
    assert_eq!((f.saves, left(t.path())), (0, 1));

    run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", true, &[]),
        Run {
            bound: &bound,
            ..Default::default()
        },
    );
    assert_eq!(profiles(&f), vec!["g"]);
    assert_eq!(f.bound, vec![(Some("old".to_string()), "g".to_string())]);
    assert_eq!(left(t.path()), 0);
}

#[test]
fn a_disabled_row_never_writes_and_keeps_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(&[], &[], McpNetMode::Off)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    assert!(
        matches!(got, Answered::Refused(ref w) if w.contains("not host-scoped")),
        "{got:?}"
    );
    assert_eq!((f.saves, left(t.path())), (0, 1));
}

/// Council B2: an attached profile under `off` is not "already set up".
#[test]
fn an_already_bound_profile_under_off_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(&[], &["g"], McpNetMode::Off)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    assert!(matches!(got, Answered::Refused(_)), "{got:?}");
    assert_eq!(left(t.path()), 1);
}

#[test]
fn no_live_entry_is_refused_and_keeps_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake::default();
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    assert!(matches!(got, Answered::Refused(_)), "{got:?}");
    assert_eq!(left(t.path()), 1);
}

/// D11: the save is checked against the card's snapshot, and the port
/// waits for the save. The non-default port makes the port assertion real.
#[test]
fn a_concurrent_change_is_stale_and_keeps_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "http://gitea.example:3000", true);
    let start = vec![live(&["gitea.example"], &[], McpNetMode::Restricted)];
    let open = || Run {
        open_port: true,
        ..Default::default()
    };

    let mut f = Fake {
        servers: start.clone(),
        race: true,
        ..Default::default()
    };
    let mut ports = Ports::default();
    assert_eq!(
        run(
            &mut f,
            &mut ports,
            t.path(),
            &r,
            &use_("g", false, &[]),
            open()
        ),
        Answered::Stale
    );
    assert!(
        ports.opened.is_empty(),
        "a port was opened for a write that did not happen"
    );

    // The card was read before another writer bound "other".
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &["other"], McpNetMode::Restricted)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut ports,
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run {
            snapshot: Some(start),
            ..open()
        },
    );
    assert_eq!((got, f.saves), (Answered::Stale, 0));
    assert!(ports.opened.is_empty());
    assert_eq!(left(t.path()), 1);
}

#[test]
fn the_port_opens_only_when_chosen_and_means_a_restart() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "http://gitea.example:3000", true);
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    let mut ports = Ports::default();
    let got = run(
        &mut f,
        &mut ports,
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run {
            open_port: true,
            ..Default::default()
        },
    );
    assert_eq!(ports.opened, vec![3000]);
    let Answered::Done { effect, notes, .. } = got else {
        panic!("{got:?}")
    };
    assert_eq!(effect, Some(Effect::Restart));
    assert!(notes.iter().any(|n| n.contains("3000")), "{notes:?}");

    let r = request(t.path(), "http://gitea.example:3000", true);
    let mut ports = Ports::default();
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    run(
        &mut f,
        &mut ports,
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    assert!(ports.opened.is_empty());
}

/// Council N6: the binding is saved, the port is not; say both and keep
/// the request so it can be answered again.
#[test]
fn a_failed_port_grant_keeps_the_request_and_says_what_was_saved() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "http://gitea.example:3000", true);
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    let mut ports = Ports {
        fail: true,
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut ports,
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run {
            open_port: true,
            ..Default::default()
        },
    );
    let Answered::PortFailed { effect, error, .. } = got else {
        panic!("{got:?}")
    };
    assert_eq!(effect, Some(Effect::NextTurn));
    assert!(error.contains("read-only"), "{error}");
    assert_eq!(profiles(&f), vec!["g"]);
    assert_eq!(left(t.path()), 1);
}

/// D12: nothing written, nothing claimed.
#[test]
fn already_set_up_writes_nothing_reports_no_effect_and_clears_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(
            &["gitea.example"],
            &["other", "g"],
            McpNetMode::Restricted,
        )],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run::default(),
    );
    let Answered::Done { effect, .. } = got else {
        panic!("{got:?}")
    };
    assert_eq!(effect, None);
    assert_eq!(profiles(&f), vec!["other", "g"], "order kept");
    assert_eq!((f.saves, f.bound.len(), left(t.path())), (0, 0, 0));
}

/// D12, council B3: a fresh login under a bound name only reaches the
/// running browser through a restart, and it is recorded.
#[test]
fn a_relogin_of_a_bound_profile_means_a_restart_and_is_recorded() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake {
        servers: vec![live(&["gitea.example"], &["g"], McpNetMode::Restricted)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &use_("g", false, &[]),
        Run {
            login_saved: Some("g"),
            ..Default::default()
        },
    );
    let Answered::Done { effect, .. } = got else {
        panic!("{got:?}")
    };
    assert_eq!(effect, Some(Effect::Restart));
    assert_eq!(f.saves, 0);
    assert_eq!(f.bound, vec![(None, "g".to_string())]);
}

#[test]
fn allow_adds_the_approved_host_for_a_no_login_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", false);
    let mut f = Fake {
        servers: vec![live(&["other.example"], &[], McpNetMode::Restricted)],
        ..Default::default()
    };
    let allow = Choice::Allow {
        approved: hosts(&["gitea.example"]),
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &allow,
        Run::default(),
    );
    let Answered::Done {
        hand_back: Some(h), ..
    } = got
    else {
        panic!("{got:?}")
    };
    assert!(!h.contains("saved login"), "{h}");
    let net = f.servers[0].network.as_ref().unwrap();
    assert!(net.allow_hosts.contains(&"gitea.example".to_string()));
    assert!(f.bound.is_empty());
}

/// D7: under `inherit` the agent's own policy decides; nothing to write.
#[test]
fn allow_under_inherit_writes_nothing_and_clears_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", false);
    let mut f = Fake {
        servers: vec![live(&[], &[], McpNetMode::Inherit)],
        ..Default::default()
    };
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &Choice::Allow { approved: vec![] },
        Run::default(),
    );
    assert!(
        matches!(got, Answered::Done { effect: None, .. }),
        "{got:?}"
    );
    assert_eq!((f.saves, left(t.path())), (0, 0));
}

#[test]
fn dismiss_only_removes_the_request() {
    let t = tempfile::tempdir().unwrap();
    let r = request(t.path(), "gitea.example", true);
    let mut f = Fake::default();
    let got = run(
        &mut f,
        &mut Ports::default(),
        t.path(),
        &r,
        &Choice::Dismiss,
        Run {
            open_port: true,
            ..Default::default()
        },
    );
    assert_eq!(
        got,
        Answered::Done {
            effect: None,
            notes: vec![],
            hand_back: None
        }
    );
    assert_eq!(left(t.path()), 0);
}

/// D4, D11: a choice is made from a row, and the row decides `--replace`.
#[test]
fn use_offer_follows_the_row() {
    let d = hosts(&["gitea.example"]);
    let ok = |o: Offer, replace| use_offer("g", &d, &o, replace);
    assert_eq!(ok(Offer::Offered, false), Ok(use_("g", false, &[])));
    assert_eq!(
        ok(Offer::AddsHosts { missing: d.clone() }, false),
        Ok(use_("g", false, &["gitea.example"]))
    );
    assert!(ok(Offer::Offered, true).unwrap_err().contains("--replace"));
    let ro = Offer::ReplaceOnly {
        with: "old".into(),
        domains: d.clone(),
        drops: hosts(&["old"]),
        adds: vec![],
    };
    assert!(ok(ro.clone(), false).unwrap_err().contains("--replace"));
    assert_eq!(ok(ro, true), Ok(use_("g", true, &[])));
    assert!(
        ok(
            Offer::Disabled {
                why: "deny_hosts blocks gitea.example".into()
            },
            false
        )
        .unwrap_err()
        .contains("deny_hosts")
    );
    assert_eq!(ok(Offer::AlreadyBound, false), Ok(use_("g", false, &[])));
}
