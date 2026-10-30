use super::*;

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_owned()).collect()
}

fn entry(args: &[&str]) -> McpServerEntry {
    let mut e: McpServerEntry = serde_yaml_ng::from_str("name: browser\ncommand: mur").unwrap();
    e.args = s(args);
    e
}

fn saved(site: &str, domains: &[&str]) -> SavedProfile {
    SavedProfile {
        site: site.to_owned(),
        domains: s(domains),
    }
}

#[test]
fn attached_profile_reads_the_launch_argv() {
    assert_eq!(attached_profile(&entry(&["browser", "record"])), None);
    assert_eq!(
        attached_profile(&entry(&["browser", "record", "--profile", "deepseek"])),
        Some("deepseek")
    );
    // A trailing flag with no value is not a profile.
    assert_eq!(attached_profile(&entry(&["--profile"])), None);
}

#[test]
fn attached_profile_reads_the_equals_form_and_stops_at_double_dash() {
    // Clap's long option also accepts `--profile=<site>` (#2203).
    assert_eq!(
        attached_profile(&entry(&["browser", "record", "--profile=deepseek"])),
        Some("deepseek")
    );
    assert_eq!(attached_profile(&entry(&["--profile="])), None);
    // After `--` every token is positional, never an option.
    assert_eq!(
        attached_profile(&entry(&[
            "browser",
            "record",
            "--",
            "--profile",
            "deepseek"
        ])),
        None
    );
    assert_eq!(
        attached_profile(&entry(&["--", "--profile=deepseek"])),
        None
    );
}

#[test]
fn domains_match_exact_sub_and_parent_but_not_lookalikes() {
    let p = [saved("deepseek", &["chat.deepseek.com"])];
    assert_eq!(matching(&p, &s(&["chat.deepseek.com"])), vec!["deepseek"]);
    assert_eq!(matching(&p, &s(&["deepseek.com"])), vec!["deepseek"]);
    assert_eq!(matching(&p, &s(&["*.deepseek.com"])), vec!["deepseek"]);
    assert_eq!(
        matching(&p, &s(&["api.chat.deepseek.com"])),
        vec!["deepseek"]
    );
    assert!(matching(&p, &s(&["evildeepseek.com"])).is_empty());
    assert!(matching(&p, &s(&["example.com"])).is_empty());
}

#[test]
fn hint_names_the_exact_live_command_with_every_host() {
    // The hint carries every allowed host, so following it — even with
    // `--replace` — never drops one already allowed.
    let p = [saved("deepseek", &["chat.deepseek.com"])];
    let hosts = s(&["cdn.example.com", "chat.deepseek.com"]);
    let h = hint(&entry(&["browser", "record"]), &hosts, &p).expect("hint");
    assert!(
        h.text
            .contains("/browser live cdn.example.com chat.deepseek.com --profile deepseek"),
        "{}",
        h.text
    );
    assert!(h.text.contains("no cookies"), "{}", h.text);
    let chip = h.chip.expect("one match offers a chip");
    assert!(format!("{chip:?}").contains("--profile deepseek"));
}

#[test]
fn no_hint_when_a_profile_is_attached_or_nothing_matches() {
    let p = [saved("deepseek", &["chat.deepseek.com"])];
    let hosts = s(&["chat.deepseek.com"]);
    let attached = entry(&["browser", "record", "--profile", "deepseek"]);
    assert!(hint(&attached, &hosts, &p).is_none());
    assert!(hint(&entry(&[]), &s(&["example.com"]), &p).is_none());
    assert!(hint(&entry(&[]), &hosts, &[]).is_none());
}

#[test]
fn several_matches_are_all_named_without_a_chip() {
    let p = [
        saved("work", &["example.com"]),
        saved("home", &["example.com"]),
    ];
    let h = hint(&entry(&[]), &s(&["example.com"]), &p).expect("hint");
    assert!(h.text.contains("--profile work"), "{}", h.text);
    assert!(h.text.contains("--profile home"), "{}", h.text);
    assert!(h.chip.is_none());
}

#[test]
fn status_line_shows_the_attached_profile_or_none() {
    assert_eq!(
        status_line(&entry(&["--profile", "deepseek"])),
        "browser profile: `deepseek`"
    );
    assert!(status_line(&entry(&[])).starts_with("browser profile: none"));
}

#[test]
fn saved_profiles_load_from_disk_skipping_incomplete_ones() {
    let home = tempfile::tempdir().unwrap();
    let write = |site: &str, meta: &str, state: bool| {
        let dir = mur_browser::paths::profile_dir(home.path(), site);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(mur_browser::paths::profile_meta(home.path(), site), meta).unwrap();
        if state {
            std::fs::write(mur_browser::paths::profile_state(home.path(), site), "x").unwrap();
        }
    };
    let at = "last_auth: 2026-01-01T00:00:00Z";
    write(
        "deepseek",
        &format!("url: https://chat.deepseek.com/login\n{at}"),
        true,
    );
    write(
        "shop",
        &format!("url: https://login.shop.test/\n{at}\nallow_domains: [shop.test]"),
        true,
    );
    write("half", &format!("url: https://half.test/\n{at}"), false);
    let mut got = load_saved_profiles(home.path());
    got.sort_by(|a, b| a.site.cmp(&b.site));
    assert_eq!(
        got,
        vec![
            saved("deepseek", &["chat.deepseek.com"]),
            saved("shop", &["shop.test"]),
        ]
    );
    // No profiles dir at all is simply "none saved".
    assert!(load_saved_profiles(&home.path().join("missing")).is_empty());
}
