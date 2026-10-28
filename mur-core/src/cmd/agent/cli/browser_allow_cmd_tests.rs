use super::super::browser_profile_hint::SavedProfile;
use super::*;
use mur_common::agent::McpServerNetwork;

#[derive(Default)]
struct Fake {
    servers: Vec<McpServerEntry>,
    saved: Option<Vec<McpServerEntry>>,
    live: Vec<(String, bool)>,
    live_err: Option<String>,
    profiles: Vec<SavedProfile>,
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
    fn saved_profiles(&mut self) -> Vec<SavedProfile> {
        self.profiles.clone()
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

fn deepseek() -> Vec<SavedProfile> {
    vec![SavedProfile {
        site: "deepseek".into(),
        domains: s(&["chat.deepseek.com"]),
    }]
}

#[test]
fn allow_hints_at_a_saved_profile_the_entry_does_not_launch_with() {
    // #2100: `mur browser setup` made the entry sessionless; allowing the
    // host must say the saved login is not attached, and how to attach it.
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.profiles = deepseek();
    let (text, chip) = run_allow("mur", &s(&["--yes", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(text.contains("always allowed"), "{text}");
    assert!(
        text.contains("/browser live chat.deepseek.com --profile deepseek"),
        "{text}"
    );
    assert!(format!("{chip:?}").contains("--profile deepseek"));
}

#[test]
fn allow_hint_also_covers_the_session_and_already_allowed_paths() {
    let mut f = fake(McpNetMode::Restricted, &["chat.deepseek.com"]);
    f.profiles = deepseek();
    let (text, _) = run_allow("mur", &s(&["--yes", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(text.contains("--profile deepseek"), "{text}");
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.profiles = deepseek();
    let (text, _) = run_allow("mur", &s(&["--session", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(text.contains("--profile deepseek"), "{text}");
}

#[test]
fn session_hint_warns_that_following_it_makes_the_host_permanent() {
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.profiles = deepseek();
    let (text, _) = run_allow("mur", &s(&["--session", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(
        text.contains("also keeps `chat.deepseek.com` allowed after a restart"),
        "{text}"
    );
    // The permanent path has nothing temporary to warn about.
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.profiles = deepseek();
    let (text, _) = run_allow("mur", &s(&["--yes", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(!text.contains("after a restart"), "{text}");
}

#[test]
fn allow_stays_quiet_when_the_profile_is_attached_or_unrelated() {
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.servers[0].args = s(&["browser", "record", "--profile", "deepseek"]);
    f.profiles = deepseek();
    let (text, _) = run_allow("mur", &s(&["--yes", "chat.deepseek.com"]), &mut f).unwrap();
    assert!(!text.contains("no saved profile"), "{text}");
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.profiles = deepseek();
    let (text, chip) = run_allow("mur", &s(&["--yes", "a.example.com"]), &mut f).unwrap();
    assert!(!text.contains("no saved profile"), "{text}");
    assert!(chip.is_none());
}

#[test]
fn status_names_the_attached_profile_and_hints_when_none() {
    let f = fake(McpNetMode::Restricted, &["chat.deepseek.com"]);
    let (text, chip) = status_tail("TABLE".into(), &f.servers, &deepseek());
    assert!(text.starts_with("TABLE\nbrowser profile: none"), "{text}");
    assert!(
        text.contains("/browser live chat.deepseek.com --profile deepseek"),
        "{text}"
    );
    assert!(chip.is_some());
    let mut f = fake(McpNetMode::Restricted, &["chat.deepseek.com"]);
    f.servers[0].args = s(&["--profile", "deepseek"]);
    let (text, chip) = status_tail("TABLE".into(), &f.servers, &deepseek());
    assert_eq!(text, "TABLE\nbrowser profile: `deepseek`");
    assert!(chip.is_none());
    // No browser entry: the table alone.
    let (text, _) = status_tail("TABLE".into(), &[], &deepseek());
    assert_eq!(text, "TABLE");
}

#[test]
fn failed_session_grant_still_prompts_before_the_next_run() {
    // PR #2184 review: a session grant that never reached the running agent
    // must not be remembered, or re-sending `/browser testing <url>` skips
    // the pre-run prompt for a host that is still blocked.
    use super::super::browser_mode_cmd::blocked_hosts;
    let args = s(&["--session", "x.com"]);
    let mut f = fake(McpNetMode::Restricted, &[]);
    f.live_err = Some("agent not reachable".into());
    let res = run_allow("mur", &args, &mut f).map_err(|e| e.to_string());
    assert!(res.is_err());
    let mut skip = std::collections::HashSet::new();
    skip.extend(granted_session_host(&args, &res));
    assert!(skip.is_empty(), "failed grant was recorded: {skip:?}");
    let entry = f.servers[0].clone();
    assert_eq!(
        blocked_hosts(&s(&["x.com"]), Some(&entry), &skip),
        vec!["x.com".to_owned()]
    );

    // The same request, succeeding, is remembered and stops the prompt.
    let mut f = fake(McpNetMode::Restricted, &[]);
    let res = run_allow("mur", &args, &mut f).map_err(|e| e.to_string());
    skip.extend(granted_session_host(&args, &res));
    assert!(blocked_hosts(&s(&["x.com"]), Some(&entry), &skip).is_empty());
}

#[test]
fn only_session_grants_are_recorded() {
    let ok: Result<Managed, String> = Ok(("done".into(), None));
    assert_eq!(granted_session_host(&s(&["x.com"]), &ok), None);
    assert_eq!(
        granted_session_host(&s(&["--session", "X.com"]), &ok),
        Some("x.com".to_owned())
    );
}
