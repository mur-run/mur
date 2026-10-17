use super::*;
use mur_common::agent::McpServerNetwork;

#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    saved: Option<Vec<McpServerEntry>>,
    live: Vec<(String, bool)>,
    live_err: Option<String>,
}

impl AllowOps for Fake {
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(self.servers.clone())
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<()> {
        self.saved = Some(servers);
        Ok(())
    }
    fn grant_live(&mut self, host: &str, session: bool) -> Result<(), String> {
        self.live.push((host.to_owned(), session));
        self.live_err.clone().map_or(Ok(()), Err)
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn fake(mode: McpNetMode, allow: &[&str]) -> Fake {
    let mut e: McpServerEntry =
        serde_yaml_ng::from_str(&format!("name: {LIVE_ENTRY}\ncommand: mur")).unwrap();
    e.network = Some(McpServerNetwork {
        mode,
        allow_hosts: s(allow),
        ..Default::default()
    });
    Fake {
        servers: vec![e],
        ..Default::default()
    }
}

fn hosts(servers: &[McpServerEntry]) -> Vec<String> {
    servers[0].network.as_ref().unwrap().allow_hosts.clone()
}

#[test]
fn parse_needs_exactly_one_valid_host() {
    assert!(parse_allow(&s(&[])).is_err());
    assert!(parse_allow(&s(&["a.com", "b.com"])).is_err());
    let e = parse_allow(&s(&["競品"])).unwrap_err();
    assert!(e.contains("nothing was changed"), "{e}");
    let a = parse_allow(&s(&["--session", "IMG.Shop.Example.com"])).unwrap();
    assert_eq!(a.host, "img.shop.example.com");
    assert!(a.session && !a.yes);
}

#[test]
fn permanent_without_yes_previews_and_writes_nothing() {
    let mut f = fake(McpNetMode::Restricted, &["shop.example.com"]);
    let (text, chip) = run_allow("mur", &s(&["img.shop.example.com"]), &mut f).unwrap();
    assert!(text.contains("+ img.shop.example.com"), "{text}");
    assert!(text.contains("  shop.example.com"), "{text}");
    assert!(text.contains("applied to the running agent now"), "{text}");
    assert!(f.saved.is_none() && f.live.is_empty());
    let chip = chip.expect("confirm chip");
    assert!(format!("{chip:?}").contains("browser allow --yes img.shop.example.com"));
}

#[test]
fn permanent_with_yes_saves_then_applies_live() {
    let mut f = fake(McpNetMode::Restricted, &["shop.example.com"]);
    let (text, chip) = run_allow("mur", &s(&["--yes", "img.shop.example.com"]), &mut f).unwrap();
    assert_eq!(
        hosts(f.saved.as_ref().unwrap()),
        s(&["shop.example.com", "img.shop.example.com"])
    );
    assert_eq!(f.live, vec![("img.shop.example.com".to_owned(), false)]);
    assert!(text.contains("applied to the running agent now"), "{text}");
    assert!(chip.is_none());
}

#[test]
fn permanent_grant_stands_when_the_agent_is_not_running() {
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.live_err = Some("agent not running".into());
    let (text, _) = run_allow("mur", &s(&["--yes", "a.example.com"]), &mut f).unwrap();
    assert!(f.saved.is_some());
    assert!(text.contains("restart it to apply"), "{text}");
}

#[test]
fn session_grant_is_live_only() {
    let mut f = fake(McpNetMode::Restricted, &[]);
    let (text, chip) = run_allow("mur", &s(&["--session", "a.example.com"]), &mut f).unwrap();
    assert!(f.saved.is_none());
    assert_eq!(f.live, vec![("a.example.com".to_owned(), true)]);
    assert!(text.contains("gone when the agent restarts"), "{text}");
    assert!(chip.is_none());
}

#[test]
fn research_mode_and_unscoped_servers_refuse() {
    let mut f = fake(McpNetMode::BroadAudited, &[]);
    let e = run_allow("mur", &s(&["--session", "a.example.com"]), &mut f).unwrap_err();
    assert!(
        e.to_string().contains("deny-listed host cannot be granted"),
        "{e}"
    );
    assert!(f.live.is_empty());
    let mut f = fake(McpNetMode::Inherit, &[]);
    assert!(run_allow("mur", &s(&["a.example.com"]), &mut f).is_err());
    let mut none = Fake::default();
    assert!(run_allow("mur", &s(&["a.example.com"]), &mut none).is_err());
}

#[test]
fn already_allowed_is_a_no_op() {
    let mut f = fake(McpNetMode::Restricted, &["a.example.com"]);
    let (text, chip) = run_allow("mur", &s(&["--yes", "a.example.com"]), &mut f).unwrap();
    assert!(text.contains("nothing changed"), "{text}");
    assert!(f.saved.is_none() && chip.is_none());
}
