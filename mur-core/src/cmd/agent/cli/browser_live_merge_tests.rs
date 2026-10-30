//! #2101: `/browser live` adds hosts by default; `--replace` scopes exactly.

use super::*;

fn restricted(hosts: &[&str]) -> McpServerNetwork {
    McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: s(hosts),
        ..Default::default()
    }
}

fn existing(hosts: &[&str]) -> Fake {
    let mut e = live_entry();
    e.network = Some(restricted(hosts));
    Fake {
        servers: vec![e],
        ..Fake::default()
    }
}

#[test]
fn merge_keeps_old_hosts_first_and_skips_duplicates() {
    let mut after = restricted(&["b.example.com", "c.example.com"]);
    let kept = merge_hosts(
        &mut after,
        Some(&restricted(&["a.example.com", "b.example.com"])),
    );
    assert_eq!(kept, s(&["a.example.com"]));
    assert_eq!(
        after.allow_hosts,
        s(&["a.example.com", "b.example.com", "c.example.com"])
    );
}

#[test]
fn merge_ignores_a_non_restricted_previous_policy() {
    let mut after = restricted(&["a.example.com"]);
    let research = research_network("u".into(), 0);
    assert!(merge_hosts(&mut after, Some(&research)).is_empty());
    assert_eq!(after.allow_hosts, s(&["a.example.com"]));
}

// The issue's repro: attaching a profile must not drop a `/browser allow` host.
#[test]
fn rerun_with_a_profile_keeps_hosts_added_by_allow() {
    let mut f = existing(&["chat.deepseek.com", "www.deepseek.com"]);
    let args = s(&["--yes", "chat.deepseek.com", "--profile", "deepseek"]);
    let (text, _) = run("bob", &args, true, &mut f).unwrap();
    assert_eq!(
        hosts_of(&f.servers),
        s(&["chat.deepseek.com", "www.deepseek.com"])
    );
    assert!(
        text.contains("kept from the previous allowlist: www.deepseek.com"),
        "{text}"
    );
    assert!(text.contains("--replace"), "{text}");
    assert!(!text.contains("replaced previous allowlist"), "{text}");
}

#[test]
fn replace_scopes_to_exactly_the_named_hosts() {
    let mut f = existing(&["old.example.com"]);
    let (text, _) = run(
        "bob",
        &s(&["--yes", "--replace", "new.example.com"]),
        true,
        &mut f,
    )
    .unwrap();
    assert_eq!(hosts_of(&f.servers), s(&["new.example.com"]));
    assert!(
        text.contains("replaced previous allowlist: old.example.com"),
        "{text}"
    );
}

#[test]
fn naming_only_hosts_already_allowed_changes_nothing() {
    let mut f = existing(&["a.example.com", "b.example.com"]);
    let (text, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none(), "{text}");
    assert_eq!(
        text,
        unchanged_text(&s(&["a.example.com", "b.example.com"]))
    );
}

#[test]
fn preview_shows_the_merged_list_and_the_chip_keeps_replace() {
    let mut f = existing(&["old.example.com"]);
    let (text, _) = run("bob", &s(&["new.example.com"]), true, &mut f).unwrap();
    assert!(text.contains("old.example.com"), "{text}");
    assert_eq!(f.saves, 0, "preview writes nothing");
    let (_, chip) = run("bob", &s(&["--replace", "new.example.com"]), true, &mut f).unwrap();
    assert_eq!(
        chip.expect("confirm chip").kind,
        mur_common::proposal::ProposalKind::Slash(
            "browser live --yes --replace new.example.com".into()
        )
    );
}

#[test]
fn replace_with_research_is_refused() {
    let mut f = Fake::default();
    let err = run("bob", &s(&["--research", "--replace"]), true, &mut f).unwrap_err();
    assert!(err.to_string().contains("--replace"), "{err}");
    assert_eq!((f.adds.len(), f.saves), (0, 0));
}
