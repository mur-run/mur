use std::fs;
use std::path::Path;

use chrono::Utc;
use mur_browser::auth::{ProfileMeta, write_meta};
use mur_browser::paths;
use mur_browser_request::{BrowserRequest, Site, read_pending};
use mur_common::AgentProfile;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

use super::*;
use crate::cmd::browser::request_card::{Card, Offer};

fn saved(home: &Path, site: &str, url: &str) {
    fs::create_dir_all(paths::profile_dir(home, site)).unwrap();
    fs::write(paths::profile_state(home, site), b"encrypted").unwrap();
    write_meta(
        &paths::profile_meta(home, site),
        &ProfileMeta {
            url: url.into(),
            last_auth: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            earliest_cookie_expires: None,
            allow_domains: Vec::new(),
            last_refresh: None,
            writeback_abandoned: None,
        },
    )
    .unwrap();
}

fn agent(home: &Path, name: &str, bound: &[&str], allow_ports: &[u16]) {
    let mut args = vec!["browser".to_string(), "live".to_string()];
    for p in bound {
        args.extend(["--profile".to_string(), p.to_string()]);
    }
    let mut e: McpServerEntry = serde_json::from_value(
        serde_json::json!({ "name": "browser", "command": "mur", "args": args }),
    )
    .unwrap();
    e.network = Some(McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: vec!["gitea.example".into()],
        ..Default::default()
    });
    let mut p = AgentProfile::default_for_tests();
    p.name = name.into();
    p.mcp_servers = vec![e];
    p.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::Restricted;
    p.entitlements.network.outbound.allow_ports = allow_ports.to_vec();
    let dir = home.join("agents").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("profile.yaml"),
        serde_yaml_ng::to_string(&p).unwrap(),
    )
    .unwrap();
}

#[test]
fn saved_profiles_skip_stateless_and_unreadable_ones() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    saved(h, "gitea", "https://gitea.example/login");
    fs::create_dir_all(paths::profile_dir(h, "half")).unwrap();
    saved(h, "broken", "https://x.example");
    fs::write(paths::profile_meta(h, "broken"), "{not yaml").unwrap();
    let names: Vec<String> = saved_profiles(h).into_iter().map(|p| p.name).collect();
    assert_eq!(names, vec!["gitea"]);
}

#[test]
fn card_for_reads_profiles_bindings_ports_and_the_snapshot_off_disk() {
    let t = tempfile::tempdir().unwrap();
    let h = t.path();
    saved(h, "gitea", "https://gitea.example/login");
    agent(h, "asker", &[], &[]);
    agent(h, "reviewer", &["gitea"], &[]);
    let home = h.join("agents").join("asker");
    let s = Site::parse("http://gitea.example:3000").unwrap();
    mur_browser_request::write(
        &home,
        &BrowserRequest::new(&s, true, "PR", Utc::now()).unwrap(),
    )
    .unwrap();
    let r = read_pending(&home, Utc::now()).remove(0);

    let (card, snapshot) = card_for(h, "asker", &r).unwrap();
    let Card::Login {
        rows,
        login_again,
        port,
    } = card
    else {
        panic!()
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].offer, Offer::Offered);
    assert_eq!(rows[0].shared_with, vec!["reviewer"]);
    assert_eq!(
        (login_again.name.as_str(), login_again.exists, port),
        ("gitea.example-3000", false, Some(3000))
    );
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0].name, "browser");

    agent(h, "asker", &[], &[3000]);
    let (Card::Login { port, .. }, _) = card_for(h, "asker", &r).unwrap() else {
        panic!()
    };
    assert_eq!(port, None);
}
