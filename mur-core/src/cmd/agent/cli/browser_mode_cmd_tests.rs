use super::*;
use mur_common::agent::McpServerNetwork;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn saved(site: &str, domains: &[&str]) -> SavedProfile {
    SavedProfile {
        site: site.to_owned(),
        domains: s(domains),
    }
}

fn entry(mode: McpNetMode, allow: &[&str]) -> McpServerEntry {
    let mut e: McpServerEntry = serde_yaml_ng::from_str("name: browser\ncommand: mur").unwrap();
    e.network = Some(McpServerNetwork {
        mode,
        allow_hosts: s(allow),
        ..Default::default()
    });
    e
}

#[test]
fn list_args_split_keywords_and_tags() {
    assert_eq!(
        parse_list(&s(&["DeepSeek", "--tag", "daily", "ask"])).unwrap(),
        ListArgs {
            keywords: s(&["deepseek", "ask"]),
            tags: s(&["daily"]),
        }
    );
    assert!(parse_list(&s(&["--tag"])).is_err());
    assert!(parse_list(&s(&["--json"])).unwrap_err().contains("--json"));
}

#[test]
fn list_on_missing_runs_dir_says_nothing_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let out = render_list(&dir.path().join("runs"), &ListArgs::default(), Utc::now());
    assert!(out.starts_with("no browser recordings yet"), "{out}");
}

#[test]
fn url_hosts_reads_only_http_urls() {
    let args = s(&[
        "ask",
        "https://chat.deepseek.com/a/b?q=1",
        "example.org",
        "ftp://files.example.com/x",
        "http://chat.deepseek.com/again",
    ]);
    assert_eq!(url_hosts(&args), s(&["chat.deepseek.com"]));
}

#[test]
fn explicit_profile_is_split_out_and_must_exist() {
    let have = [saved("deepseek", &["chat.deepseek.com"])];
    let plan = plan_turn(&s(&["automation", "--profile", "deepseek", "ask"]), &have).unwrap();
    assert_eq!(plan.args, s(&["automation", "ask"]));
    assert_eq!(plan.profile.as_deref(), Some("deepseek"));

    let err = plan_turn(&s(&["testing", "--profile", "nope"]), &have).unwrap_err();
    assert!(
        err.contains("`nope`") && err.contains("`deepseek`"),
        "{err}"
    );
    assert!(plan_turn(&s(&["testing", "--profile"]), &have).is_err());
}

#[test]
fn one_matching_profile_is_picked_from_the_url() {
    let have = [
        saved("deepseek", &["chat.deepseek.com"]),
        saved("shop", &["shop.example.com"]),
    ];
    let plan = plan_turn(&s(&["automation", "https://chat.deepseek.com/"]), &have).unwrap();
    assert_eq!(plan.profile.as_deref(), Some("deepseek"));
    assert!(plan.note.unwrap().contains("`deepseek`"));
    // No URL, no guess.
    let plan = plan_turn(&s(&["automation", "ask deepseek"]), &have).unwrap();
    assert_eq!(plan.profile, None);
    assert_eq!(plan.note, None);
}

#[test]
fn several_matching_profiles_pick_none_and_say_so() {
    let have = [
        saved("work", &["chat.deepseek.com"]),
        saved("home", &["deepseek.com"]),
    ];
    let plan = plan_turn(&s(&["testing", "https://chat.deepseek.com/"]), &have).unwrap();
    assert_eq!(plan.profile, None);
    let note = plan.note.unwrap();
    assert!(note.contains("`work`") && note.contains("`home`"), "{note}");
}

#[test]
fn instruction_names_the_profile_for_record_and_replay() {
    let plan = TurnPlan {
        args: s(&["automation", "ask"]),
        profile: Some("deepseek".into()),
        note: None,
    };
    let text = instruction(&plan);
    assert!(text.starts_with("Use the browser skill. automation ask"));
    assert!(text.contains("--profile deepseek"));
    let bare = TurnPlan {
        args: vec![],
        profile: None,
        note: None,
    };
    assert_eq!(instruction(&bare), "Use the browser skill.");
}

#[test]
fn blocked_hosts_only_checks_a_restricted_entry() {
    let hosts = s(&["chat.deepseek.com", "x.com", "img.example.org"]);
    let none = HashSet::new();
    let e = entry(
        McpNetMode::Restricted,
        &["chat.deepseek.com", "*.example.org"],
    );
    assert_eq!(blocked_hosts(&hosts, Some(&e), &none, &none), s(&["x.com"]));
    // Session grants and dismissals are not asked again.
    let skip: HashSet<String> = ["x.com".to_owned()].into();
    assert!(blocked_hosts(&hosts, Some(&e), &skip, &none).is_empty());
    assert!(blocked_hosts(&hosts, Some(&e), &none, &skip).is_empty());
    // No entry, or research mode: nothing to ask.
    assert!(blocked_hosts(&hosts, None, &none, &none).is_empty());
    let broad = entry(McpNetMode::BroadAudited, &[]);
    assert!(blocked_hosts(&hosts, Some(&broad), &none, &none).is_empty());
}

#[test]
fn allow_prompt_offers_the_three_egress_chips() {
    assert!(allow_prompt(&[]).is_none());
    let (text, chips) = allow_prompt(&s(&["x.com", "y.com"])).unwrap();
    assert!(text.contains("`x.com`") && text.contains("y.com"), "{text}");
    let cmds: Vec<&str> = chips.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(
        cmds,
        [
            "/browser allow --session x.com",
            "/browser allow x.com",
            "/browser allow --dismiss x.com",
        ]
    );
}

#[test]
fn wildcard_session_grant_covers_subdomains_but_not_unrelated_hosts() {
    // PR #2184 review: `/browser allow --session *.example.com` must stop the
    // pre-run prompt for every covered host, as the running proxy does.
    let hosts = s(&["app.example.com", "example.com", "x.com"]);
    let e = entry(McpNetMode::Restricted, &[]);
    let none = HashSet::new();
    let grants: HashSet<String> = ["*.example.com".to_owned()].into();
    assert_eq!(
        blocked_hosts(&hosts, Some(&e), &none, &grants),
        s(&["x.com"])
    );
    // A dismissal stays exact: it names the one host the user declined.
    let dismissed: HashSet<String> = ["app.example.com".to_owned()].into();
    assert_eq!(
        blocked_hosts(&hosts, Some(&e), &dismissed, &none),
        s(&["example.com", "x.com"])
    );
}
