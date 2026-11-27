//! `/browser live` confirm flow (#1905): preview vs. `--yes`, when a write
//! applies, the headless-shell grant, research mode, and deny-host carry-over.

use super::*;

// Rerun with the same hosts and nothing for `save` to fix: no restart chip,
// and the reply says so instead of "profile updated".
#[test]
fn unchanged_rerun_offers_no_restart() {
    let mut f = Fake::default();
    run("bob", &s(&["--yes", "a.example.com"]), true, &mut f).unwrap();
    let (text, chip) = run("bob", &s(&["--yes", "a.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none(), "{text}");
    assert!(text.contains("nothing changed"), "{text}");
    // A different allowlist is a real change again — and a hot one.
    let (text, chip) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none(), "{text}");
    assert!(text.contains("next turn uses this"), "{text}");
}

// Hot reload D1: editing the existing entry's hosts needs no restart, and the
// reply says the open tab goes away.
#[test]
fn a_host_edit_applies_next_turn_and_says_the_tab_closes() {
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["--yes", "a.example.com"]), true, &mut f).unwrap();
    assert_eq!(
        chip,
        Some(Proposal::restart(RESTART_LABEL)),
        "new entry: {text}"
    );
    assert!(text.contains("Restart the agent"), "{text}");
    assert!(text.contains("after the agent restarts"), "{text}");
    // After a real write the past tense is true (#2406).
    assert!(text.contains("Saved to the profile"), "{text}");
    assert!(!text.contains("Will be saved"), "{text}");

    let (text, chip) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none(), "{text}");
    assert!(text.contains("tab will be closed"), "{text}");
    assert!(text.contains("next turn uses it"), "preview line: {text}");
    assert!(text.contains("no restart needed"), "{text}");
    assert!(
        !text.contains("Restart the agent") && !text.contains("after the agent restarts"),
        "{text}"
    );
}

// A write that adds a grant, or switches research mode, stays a restart.
#[test]
fn a_grant_or_a_mode_switch_still_needs_a_restart() {
    let mut e = live_entry();
    e.network = Some(live_network(&s(&["a.example.com"])));
    let mut f = Fake {
        servers: vec![e.clone()],
        save_notes: vec!["granted write on /x/browser".into()],
        ..Fake::default()
    };
    let (text, chip) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    assert_eq!(chip, Some(Proposal::restart(RESTART_LABEL)), "{text}");

    let mut f = Fake {
        servers: vec![e],
        ..Fake::default()
    };
    let (text, chip) = run("bob", &s(&["--yes", "--research"]), true, &mut f).unwrap();
    assert_eq!(chip, Some(Proposal::restart(RESTART_LABEL)), "{text}");
    assert!(text.contains("Restart the agent"), "{text}");
}

// The confirm preview (no `--yes`) predicts the same: hot for a same-mode
// host edit, restart otherwise.
#[test]
fn the_preview_says_when_the_write_will_apply() {
    let mut e = live_entry();
    e.network = Some(live_network(&s(&["a.example.com"])));
    let mut f = Fake {
        servers: vec![e],
        ..Fake::default()
    };
    let (text, _) = run("bob", &s(&["b.example.com"]), true, &mut f).unwrap();
    assert!(text.contains("the agent's next turn uses it"), "{text}");
    // #2406: the confirm preview has written nothing yet.
    assert!(!text.contains("Saved to the profile"), "{text}");
    assert!(text.contains("Will be saved to the profile"), "{text}");
    let (text, _) = run("bob", &s(&["--research"]), true, &mut f).unwrap();
    assert!(text.contains("after the agent restarts"), "{text}");
    let mut f = Fake::default();
    let (text, _) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(text.contains("after the agent restarts"), "{text}");
}

// #1639: the sealed live server execs Playwright's headless shell, which
// lives outside every allowlisted binary — the grant must be its build dir.
#[test]
fn live_spawn_dir_is_the_newest_complete_headless_build() {
    let d = tempfile::tempdir().unwrap();
    for (name, done) in [
        ("chromium_headless_shell-1217", true),
        ("chromium_headless_shell-1246", true),
        ("chromium_headless_shell-1300", false),
    ] {
        let exe = d
            .path()
            .join(name)
            .join("chrome-headless-shell-mac-arm64/chrome-headless-shell");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "").unwrap();
        if done {
            std::fs::write(d.path().join(name).join("INSTALLATION_COMPLETE"), "").unwrap();
        }
    }
    let want = d.path().join("chromium_headless_shell-1246");
    assert_eq!(
        live_spawn_dir(Some(d.path())),
        Some(want.to_string_lossy().into_owned())
    );
}

#[test]
fn live_spawn_dir_is_none_without_a_shell() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(live_spawn_dir(Some(d.path())), None);
    assert_eq!(live_spawn_dir(None), None);
}

// #1905 Part 2: the first call only previews; nothing is written until `--yes`.
#[test]
fn first_call_previews_and_writes_nothing() {
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(f.adds.is_empty() && f.saves == 0);
    assert!(text.contains("+ a.example.com"), "{text}");
    let chip = chip.expect("confirm chip");
    assert_eq!(
        chip.kind,
        mur_common::proposal::ProposalKind::Slash("browser live --yes a.example.com".into())
    );
    let vetted = mur_common::proposal::vet(&serde_json::json!({
        "label": chip.label, "kind": "slash", "command": "browser live --yes a.example.com"
    }));
    assert!(vetted.is_ok(), "{vetted:?}");
}

#[test]
fn yes_writes_and_note_includes_preview() {
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["--yes", "a.example.com"]), true, &mut f).unwrap();
    assert_eq!(f.adds.len(), 1);
    assert!(text.contains("+ a.example.com"), "{text}");
    assert_eq!(chip, Some(Proposal::restart(RESTART_LABEL)));
}

#[test]
fn unchanged_rerun_without_yes_says_unchanged_and_offers_nothing() {
    let mut e = live_entry();
    e.network = Some(live_network(&s(&["a.example.com"])));
    let mut f = Fake {
        servers: vec![e],
        ..Fake::default()
    };
    let (text, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert_eq!(text, unchanged_text(&s(&["a.example.com"])));
    assert!(chip.is_none());
    assert!(f.adds.is_empty() && f.saves == 0);
}

#[test]
fn confirm_chip_carries_normalized_hosts_and_profile() {
    let mut f = Fake::default();
    let args = s(&["https://A.example.com/x", "--profile", "site"]);
    let (_, chip) = run("bob", &args, true, &mut f).unwrap();
    assert_eq!(
        chip.expect("chip").kind,
        mur_common::proposal::ProposalKind::Slash(
            "browser live --yes a.example.com --profile site".into()
        )
    );
}

// #1905 Part 1b: `--research` previews the generated caveat and writes nothing.
#[test]
fn research_without_yes_previews_caveat_only() {
    use super::super::super::browser_live_preview::research_caveat;
    let mut f = Fake::default();
    let (text, chip) = run("bob", &s(&["--research"]), true, &mut f).unwrap();
    assert!(f.adds.is_empty() && f.saves == 0);
    assert!(
        text.contains(research_caveat(DENY_OVERLAY_ENFORCEMENT)),
        "{text}"
    );
    assert_eq!(
        chip.expect("chip").kind,
        mur_common::proposal::ProposalKind::Slash("browser live --yes --research".into())
    );
}

#[test]
fn research_with_yes_writes_broad_audited_with_authorization() {
    let mut f = Fake::default();
    let (_, chip) = run("bob", &s(&["--yes", "--research"]), true, &mut f).unwrap();
    let net = f.servers[0].network.as_ref().expect("policy");
    assert_eq!(net.mode, McpNetMode::BroadAudited);
    assert!(net.allow_hosts.is_empty());
    assert!(net.authorization.is_some());
    assert_eq!(f.research_events, 1);
    assert!(chip.is_some());
}

#[test]
fn research_with_hosts_is_refused() {
    let mut f = Fake::default();
    let err = run("bob", &s(&["--research", "a.com"]), true, &mut f).unwrap_err();
    assert!(
        err.to_string().contains("research mode takes no hosts"),
        "{err}"
    );
    assert!(f.adds.is_empty() && f.saves == 0);
}

// Review of #1919: the stored IPv6 value must be the string the egress proxy
// matches — the host part of a CONNECT authority, which is bracketed.
#[test]
fn ipv6_allowlist_matches_what_the_proxy_sees() {
    let p = parse_hosts(&s(&["[::1]:443", "2001:db8::1"])).unwrap();
    for (host, connect) in p.hosts.iter().zip(["[::1]:443", "[2001:db8::1]:443"]) {
        let seen = connect.rsplit_once(':').unwrap().0;
        assert!(
            mur_common::net::host_allowed(seen, std::slice::from_ref(host)),
            "{host} must match CONNECT {connect}"
        );
    }
}

// Review of #1919: an older entry that `save` would still repair (stale pin,
// missing grant) is not "unchanged" — preview it and offer the confirm chip.
#[test]
fn same_policy_with_pending_repairs_still_previews_and_offers_confirm() {
    let mut e = live_entry();
    e.network = Some(live_network(&s(&["a.example.com"])));
    let mut f = Fake {
        servers: vec![e],
        repairs: vec!["re-pinned `browser` to /x/mur".into()],
        ..Fake::default()
    };
    let (text, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(!text.contains("nothing changed"), "{text}");
    assert!(text.contains("re-pinned `browser`"), "{text}");
    assert_eq!(chip.map(|c| c.label), Some(CONFIRM_LABEL.to_owned()));
    assert!(f.adds.is_empty() && f.saves == 0, "preview must not write");
}

#[test]
fn research_rerun_keeps_existing_deny_hosts() {
    let existing = crate::cmd::agent::mcp_network::broad_audited_network(
        vec!["evil.example".into()],
        "dave".into(),
        1,
    );
    let mut after = research_network("dave".into(), 2);
    carry_deny_hosts(&mut after, Some(&existing));
    assert_eq!(after.deny_hosts, vec!["evil.example"]);
    assert!(same_policy(&existing, &after));
}

#[test]
fn restricted_policy_does_not_inherit_deny_hosts() {
    let existing = crate::cmd::agent::mcp_network::broad_audited_network(
        vec!["evil.example".into()],
        "dave".into(),
        1,
    );
    let mut after = live_network(&["a.com".to_owned()]);
    carry_deny_hosts(&mut after, Some(&existing));
    assert!(after.deny_hosts.is_empty());
}
