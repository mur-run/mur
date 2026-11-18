use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use mur_browser::auth::{ProfileMeta, write_meta};
use mur_browser_request::BrowserRequest;
use mur_common::AgentProfile;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;
use crate::cmd::browser::live_effect::Effect;

const SITE: &str = "http://gitea.example:3000";

fn saved(home: &Path, name: &str, last_refresh: Option<chrono::DateTime<Utc>>) {
    fs::create_dir_all(paths::profile_dir(home, name)).unwrap();
    fs::write(paths::profile_state(home, name), b"encrypted").unwrap();
    write_meta(
        &paths::profile_meta(home, name),
        &ProfileMeta {
            url: "https://gitea.example/login".into(),
            last_auth: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            earliest_cookie_expires: None,
            allow_domains: Vec::new(),
            last_refresh,
            writeback_abandoned: None,
        },
    )
    .unwrap();
}

/// An agent with a host-scoped live entry and an open request for [`SITE`].
fn asker(home: &Path, login: bool) {
    let mut e: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": ["browser", "live"] }),
    )
    .unwrap();
    e.network = Some(McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: vec!["other.example".into()],
        ..Default::default()
    });
    let mut p = AgentProfile::default_for_tests();
    p.name = "asker".into();
    p.mcp_servers = vec![e];
    p.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::Restricted;
    let dir = home.join("agents").join("asker");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
    let s = Site::parse(SITE).unwrap();
    mur_browser_request::write(
        &dir,
        &BrowserRequest::new(&s, login, "PR", Utc::now()).unwrap(),
    )
    .unwrap();
}

/// Holds whatever entries the test gives it; never saves.
struct Ops(Vec<McpServerEntry>);

impl LiveOps for Ops {
    fn servers(&mut self) -> Result<Vec<McpServerEntry>> {
        Ok(self.0.clone())
    }
    fn mcp_add(&mut self, _: &[String], _: McpServerNetwork) -> Result<String> {
        panic!("the card never creates an entry")
    }
    fn save(&mut self, _: Vec<McpServerEntry>) -> Result<(Vec<String>, Effect)> {
        panic!("not saved in these tests")
    }
    fn save_if_unchanged(
        &mut self,
        _: &[McpServerEntry],
        _: Vec<McpServerEntry>,
    ) -> Result<Option<(Vec<String>, Effect)>> {
        Ok(None)
    }
    fn pending_repairs(&mut self, _: &[McpServerEntry]) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn record_research(&mut self, _: &str) -> Result<()> {
        Ok(())
    }
    fn record_profile_bound(&mut self, _: &str, _: Option<&str>, _: &str) -> Result<()> {
        Ok(())
    }
}

struct NoPorts;
impl PortOps for NoPorts {
    fn open(&mut self, _: u16) -> Result<String> {
        panic!("the front never opens a port")
    }
}

fn site() -> Site {
    Site::parse(SITE).unwrap()
}

#[test]
fn show_without_a_request_points_at_mur_browser_requests() {
    let t = tempfile::tempdir().unwrap();
    let err = show(t.path(), "asker", &site()).unwrap_err().to_string();
    assert!(err.contains("mur browser requests"), "{err}");
}

#[test]
fn show_reads_the_card_and_the_port_step_says_any_host() {
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "gitea", None);
    asker(t.path(), true);
    let s = show(t.path(), "asker", &site()).unwrap();
    assert!(matches!(s.card, Card::Login { .. }));
    assert_eq!(s.port(), Some(3000));
    let step = port_step(&s).unwrap();
    assert_eq!(step.port, 3000);
    assert!(step.warning.contains("ANY host"), "{}", step.warning);
    // Each copied line is a complete command: an answer choice plus
    // --open-port, no prose (D1).
    assert_eq!(
        step.commands,
        vec![
            "mur browser answer asker http://gitea.example:3000 --profile gitea --open-port",
            "mur browser answer asker http://gitea.example:3000 --login --open-port",
        ]
    );
}

#[test]
fn a_replace_only_login_port_line_carries_replace_and_a_disabled_one_is_dropped() {
    // #2461: the port step's login line must bind the way the card does.
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "gitea", None);
    asker(t.path(), true);
    let s = show(t.path(), "asker", &site()).unwrap();
    let with = |login_offer: Offer| {
        let Card::Login { login_again, .. } = s.card.clone() else {
            panic!("login card")
        };
        let card = Card::Login {
            rows: Vec::new(),
            login_again: LoginAgain {
                exists: true,
                ..login_again
            },
            login_offer,
            port: Some(3000),
        };
        port_step(&Shown::for_test("asker", s.request.clone(), card))
            .unwrap()
            .commands
    };
    assert_eq!(
        with(Offer::ReplaceOnly {
            with: "gitea".into(),
            domains: vec!["gitea.example".into()],
            drops: vec!["vercel".into()],
            adds: Vec::new(),
        }),
        vec![
            "mur browser answer asker http://gitea.example:3000 --login --reauth --replace --open-port"
        ]
    );
    assert!(with(Offer::Disabled { why: "no".into() }).is_empty());
}

#[test]
fn a_reach_port_step_offers_allow_and_a_disabled_one_offers_nothing() {
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "gitea", None);
    asker(t.path(), false);
    let s = show(t.path(), "asker", &site()).unwrap();
    let step = port_step(&s).unwrap();
    assert_eq!(
        step.commands,
        vec!["mur browser answer asker http://gitea.example:3000 --allow --open-port"]
    );
    let off = Shown::for_test(
        "asker",
        s.request.clone(),
        Card::Reach {
            reach: Reach::Disabled { why: "no".into() },
            port: Some(3000),
        },
    );
    assert!(port_step(&off).unwrap().commands.is_empty());
}

#[test]
fn a_replace_only_row_needs_the_explicit_replace() {
    let c = Card::Login {
        rows: vec![ProfileRow {
            name: "b".into(),
            domains: vec!["g.example".into()],
            shared_with: vec![],
            session_elsewhere: None,
            offer: Offer::ReplaceOnly {
                with: "old".into(),
                domains: vec!["g.example".into()],
                drops: vec!["old".into()],
                adds: vec![],
            },
        }],
        login_again: LoginAgain {
            name: "x".into(),
            exists: false,
        },
        login_offer: Offer::Offered,
        port: None,
    };
    let row = |replace| Pick::Row {
        name: "b".into(),
        replace,
    };
    assert!(
        choice_for(&c, &row(false))
            .unwrap_err()
            .contains("--replace")
    );
    assert!(matches!(
        choice_for(&c, &row(true)),
        Ok(Choice::Use { replace: true, .. })
    ));
    assert!(
        choice_for(&c, &Pick::Allow)
            .unwrap_err()
            .contains("needs a login")
    );
}

#[tokio::test]
async fn entries_changed_since_show_is_stale_and_keeps_the_request() {
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "gitea", None);
    asker(t.path(), true);
    let s = show(t.path(), "asker", &site()).unwrap();
    let got = settle_with(
        t.path(),
        &s,
        Pick::Row {
            name: "gitea".into(),
            replace: false,
        },
        false,
        async |_: &LoginPlan, _: &mut (dyn FnMut(String) + Send)| unreachable!(),
        &mut |_| {},
        &mut Ops(Vec::new()),
        &mut NoPorts,
    )
    .await
    .unwrap();
    assert_eq!(got.answered, Answered::Stale);
    assert_eq!(pending(t.path(), "asker").len(), 1);
}

/// D6: closing the card cancels the login; no profile is created.
#[tokio::test]
async fn a_cancelled_login_saves_nothing() {
    let t = tempfile::tempdir().unwrap();
    asker(t.path(), true);
    let s = show(t.path(), "asker", &site()).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    drop(tx);
    let err = settle_with(
        t.path(),
        &s,
        Pick::Login {
            name: None,
            reauth: false,
            replace: false,
        },
        false,
        async |_: &LoginPlan, _: &mut (dyn FnMut(String) + Send)| {
            rx.await.map_err(|_| anyhow::anyhow!("the card closed"))
        },
        &mut |_| {},
        &mut Ops(Vec::new()),
        &mut NoPorts,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("the card closed"), "{err}");
    assert!(!paths::profile_dir(t.path(), "gitea.example-3000").exists());
    assert_eq!(pending(t.path(), "asker").len(), 1);
}

#[test]
fn status_is_saved_not_tried_until_an_agent_writes_back() {
    let t = tempfile::tempdir().unwrap();
    saved(t.path(), "fresh", None);
    let at = chrono::DateTime::from_timestamp(1_700_050_000, 0).unwrap();
    saved(t.path(), "used", Some(at));
    assert_eq!(status(t.path(), "fresh").unwrap(), Status::SavedNotTried);
    assert_eq!(status(t.path(), "used").unwrap(), Status::AgentReported(at));
    assert!(status(t.path(), "../x").is_err());
}
