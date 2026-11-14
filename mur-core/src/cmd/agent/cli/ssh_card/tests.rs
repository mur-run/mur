//! The card over a fake plan: step flow, the verdict table as Approve sees
//! it, the separate spawn consent, hand-back, and the band slot.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mur_common::agent::SshKnownHost;
use mur_ssh_request::{PendingRequest, RequestHost, SshRequest};

use super::state::{CardState, KeyAction, Origin, Step};
use super::*;
use crate::cmd::agent::cli::app::{App, SlashCmd};
use crate::cmd::agent::ssh::resolve::ScannedKey;
use crate::cmd::agent::ssh::setup::probe::AgentKey;
use crate::cmd::agent::ssh::setup::{SetupPlan, SpawnChange, Verdict};

/// A well-formed fingerprint, distinct per `c` (a paste is checked with
/// `is_fingerprint`, which a repeated letter does not pass).
fn fp(c: char) -> String {
    mur_common::agent::fingerprint_of_blob(c.to_string().as_bytes())
}

fn scanned(c: char) -> ScannedKey {
    ScannedKey {
        fingerprint: fp(c),
        entry: SshKnownHost {
            host: "gh.example".into(),
            key_type: "ssh-ed25519".into(),
            key_base64: format!("AAAA{c}"),
        },
    }
}

fn key(c: char, path: Option<&str>) -> AgentKey {
    AgentKey {
        fingerprint: fp(c),
        comment: format!("me-{c}"),
        path: path.map(Into::into),
    }
}

fn plan(verdict: Verdict, spawn: Option<SpawnChange>) -> SetupPlan {
    let matched = if verdict == Verdict::Trusted {
        vec![fp('H')]
    } else {
        vec![]
    };
    SetupPlan {
        agent: "qa".into(),
        host: RequestHost::parse("gh.example", None).unwrap(),
        user: Some("git".into()),
        keys_loaded: vec![key('K', None)],
        keys_unloaded: vec![key('U', Some("/home/u/.ssh/id_u.pub"))],
        scanned: vec![scanned('H'), scanned('J')],
        verdict,
        matched,
        spawn,
        exec_surface: None,
    }
}

fn press(c: &mut CardState, code: KeyCode) -> KeyAction {
    c.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn approve(c: &mut CardState) -> Result<crate::cmd::agent::ssh::setup::Approval, String> {
    c.step = Step::Summary;
    match press(c, KeyCode::Enter) {
        KeyAction::Approve(a) => Ok(a),
        _ => Err(c.note.clone().unwrap_or_default()),
    }
}

fn pending(reason: &str) -> PendingRequest {
    let dir = tempfile::tempdir().unwrap();
    let t = RequestHost::parse("gh.example", None).unwrap();
    let req = SshRequest::new(&t, Some("git"), reason, Utc::now()).unwrap();
    mur_ssh_request::write(dir.path(), &req).unwrap();
    mur_ssh_request::read_pending(dir.path(), Utc::now()).remove(0)
}

#[test]
fn trusted_grants_only_the_matched_key() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    let a = approve(&mut c).unwrap();
    assert_eq!(a.key_fp, fp('K'));
    let hosts: Vec<_> = a.hosts.iter().map(|h| h.host_key_fp.clone()).collect();
    assert_eq!(hosts, vec![fp('H')]);
    assert!(a.restart);
}

#[test]
fn unknown_needs_a_tick_or_paste_and_grants_only_that() {
    let unknown = Verdict::Unknown { downgraded: false };
    let mut c = CardState::new(plan(unknown.clone(), None), Origin::Direct);
    assert!(approve(&mut c).is_err(), "nothing compared yet");

    let mut c = CardState::new(plan(unknown.clone(), None), Origin::Direct);
    c.step = Step::Host;
    press(&mut c, KeyCode::Down);
    press(&mut c, KeyCode::Char(' '));
    let a = approve(&mut c).unwrap();
    assert_eq!(a.hosts.len(), 1);
    assert_eq!(a.hosts[0].host_key_fp, fp('J'));

    let mut c = CardState::new(plan(unknown, None), Origin::Direct);
    c.step = Step::Host;
    c.paste(&fp('H'));
    let a = approve(&mut c).unwrap();
    assert_eq!(a.hosts[0].host_key_fp, fp('H'));
}

#[test]
fn differs_offers_no_ticks_and_takes_only_a_paste() {
    let mut c = CardState::new(plan(Verdict::Differs, None), Origin::Direct);
    c.step = Step::Host;
    press(&mut c, KeyCode::Char(' '));
    assert!(c.ticked.is_empty(), "no tick on differs");
    assert!(c.note.as_deref().unwrap().contains("paste"));
    assert!(approve(&mut c).is_err());
    c.paste(&fp('J'));
    let a = approve(&mut c).unwrap();
    assert_eq!(a.hosts[0].host_key_fp, fp('J'));
}

#[test]
fn a_paste_the_host_never_offered_is_refused() {
    let mut c = CardState::new(plan(Verdict::Differs, None), Origin::Direct);
    c.paste(&fp('Z'));
    let e = approve(&mut c).unwrap_err();
    assert!(e.contains("did not offer"), "{e}");
}

#[test]
fn garbage_paste_is_not_taken() {
    let mut c = CardState::new(plan(Verdict::Differs, None), Origin::Direct);
    c.paste("hello");
    assert!(c.pasted.is_none());
    assert!(c.note.is_some());
}

#[test]
fn none_to_strict_needs_its_own_consent() {
    let mut c = CardState::new(
        plan(Verdict::Trusted, Some(SpawnChange::NoneToStrict)),
        Origin::Direct,
    );
    let e = approve(&mut c).unwrap_err();
    assert!(e.contains("consent"), "{e}");
    press(&mut c, KeyCode::Char('c'));
    let a = approve(&mut c).unwrap();
    assert!(a.spawn_consented);
    assert_eq!(a.spawn, Some(SpawnChange::NoneToStrict));
}

#[test]
fn add_ssh_rides_the_main_consent() {
    let mut c = CardState::new(
        plan(Verdict::Trusted, Some(SpawnChange::AddSsh)),
        Origin::Direct,
    );
    let a = approve(&mut c).unwrap();
    assert_eq!(a.spawn, Some(SpawnChange::AddSsh));
}

#[test]
fn choosing_an_unloaded_key_hands_the_terminal_to_ssh_add() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    press(&mut c, KeyCode::Down);
    match press(&mut c, KeyCode::Enter) {
        KeyAction::LoadKey(p) => assert_eq!(p, std::path::Path::new("/home/u/.ssh/id_u")),
        _ => panic!("expected ssh-add"),
    }
    assert_eq!(
        c.step,
        Step::Key,
        "stays on the key step until ssh-add proves it"
    );
}

#[test]
fn a_key_loaded_by_ssh_add_is_shown_loaded_and_stays_chosen() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    press(&mut c, KeyCode::Down);
    assert!(matches!(
        press(&mut c, KeyCode::Enter),
        KeyAction::LoadKey(_)
    ));
    // ssh-add succeeded: the agent now lists U first, and no `.pub` is left over.
    c.refresh_keys(vec![key('U', None), key('K', None)], vec![]);
    assert_eq!(
        c.chosen_key().map(|(k, l)| (k.fingerprint.clone(), l)),
        Some((fp('U'), true))
    );
    assert_eq!(c.step, Step::Host, "a loaded key moves the card on");
    assert_eq!(approve(&mut c).unwrap().key_fp, fp('U'));
}

/// Spec "Error handling": a wrong passphrase or failed ssh-add keeps the
/// card on the key step, and nothing grants a key ssh-agent does not hold.
#[test]
fn a_failed_ssh_add_stays_on_the_key_step_and_grants_nothing() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    press(&mut c, KeyCode::Down);
    assert!(matches!(
        press(&mut c, KeyCode::Enter),
        KeyAction::LoadKey(_)
    ));
    c.refresh_keys(
        vec![key('K', None)],
        vec![key('U', Some("/home/u/.ssh/id_u.pub"))],
    );
    assert_eq!(c.step, Step::Key);
    assert!(c.note.as_deref().unwrap_or("").contains("did not load"));
    let e = approve(&mut c).unwrap_err();
    assert!(e.contains("not loaded"), "{e}");
}

#[test]
fn keys_right_after_an_auto_open_are_dropped() {
    let mut app = App::test_fixture();
    app.ssh.card = Some(Box::new(CardState::new(
        plan(Verdict::Trusted, Some(SpawnChange::NoneToStrict)),
        Origin::Direct,
    )));
    app.ssh.opened_at = Some(std::time::Instant::now());
    assert!(
        !app.ssh.settled(),
        "a stray key inside the window is not the card's"
    );
    app.ssh.opened_at = Some(std::time::Instant::now() - SETTLE);
    assert!(app.ssh.settled());
    app.ssh.opened_at = None;
    assert!(app.ssh.settled());
}

#[test]
fn consent_toggle_is_inert_without_none_to_strict() {
    let mut c = CardState::new(
        plan(Verdict::Trusted, Some(SpawnChange::AddSsh)),
        Origin::Direct,
    );
    c.step = Step::Summary;
    press(&mut c, KeyCode::Char('c'));
    assert!(!c.spawn_consented);
}

#[test]
fn a_refresh_that_drops_the_chosen_key_falls_back_to_the_first() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    c.key_idx = 1;
    c.refresh_keys(vec![key('K', None)], vec![]);
    assert_eq!(c.key_idx, 0);
    c.refresh_keys(vec![], vec![]);
    assert_eq!(c.key_idx, 0);
    assert!(c.chosen_key().is_none());
}

#[test]
fn steps_walk_forward_and_back_and_esc_closes() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    press(&mut c, KeyCode::Enter);
    assert_eq!(c.step, Step::Host);
    press(&mut c, KeyCode::Enter);
    assert_eq!(c.step, Step::Summary);
    press(&mut c, KeyCode::Left);
    assert_eq!(c.step, Step::Host);
    assert!(matches!(press(&mut c, KeyCode::Esc), KeyAction::Close));
}

#[test]
fn a_bad_login_user_blocks_approve() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    press(&mut c, KeyCode::Char('u'));
    for ch in " x".chars() {
        press(&mut c, KeyCode::Char(ch));
    }
    press(&mut c, KeyCode::Enter);
    assert!(approve(&mut c).is_err());
}

#[test]
fn hand_back_follows_the_origin() {
    assert_eq!(Origin::Direct.hand_back(), None);
    let r = pending("deploy the site");
    let from_list = Origin::Pending(r.clone()).hand_back().unwrap();
    assert!(from_list.contains("deploy the site"));
    assert!(from_list.contains("test connection"));
    let tool = Origin::Tool {
        request: r.clone(),
        resend: Some("ship it".into()),
    };
    assert_eq!(tool.hand_back().as_deref(), Some("ship it"));
    let tool_no_text = Origin::Tool {
        request: r,
        resend: None,
    };
    assert!(
        tool_no_text
            .hand_back()
            .unwrap()
            .contains("test connection")
    );
}

#[test]
fn reason_is_shown_as_the_agents_words() {
    let c = CardState::new(
        plan(Verdict::Trusted, None),
        Origin::Pending(pending("rotate logs")),
    );
    let theme = App::test_fixture().theme;
    let text: Vec<String> = render::lines(&c, theme)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert!(text[0].contains("its own words"));
    assert!(text[1].contains("│ rotate logs"));
}

#[test]
fn slash_ssh_parses() {
    use crate::cmd::agent::cli::app::parse_slash;
    assert_eq!(parse_slash("/ssh"), Some(SlashCmd::Ssh(vec![])));
    assert_eq!(
        parse_slash("/ssh gh.example:2222 git"),
        Some(SlashCmd::Ssh(vec!["gh.example:2222".into(), "git".into()]))
    );
}

#[test]
fn host_port_split() {
    assert_eq!(split_port("h:2222"), ("h", Some(2222)));
    assert_eq!(split_port("h"), ("h", None));
    assert_eq!(split_port("::1"), ("::1", None));
    assert_eq!(split_port("[::1]:2222"), ("[::1]", Some(2222)));
    assert_eq!(split_port("[::1]"), ("[::1]", None));
    let (h, p) = split_port("[::1]:2222");
    assert_eq!(RequestHost::parse(h, p).unwrap().port(), 2222);
}

/// #2370 review: the card's rows count wrapped lines, and a short band
/// drops from the top so the consent box and Approve note stay visible.
#[test]
fn wrapped_lines_are_counted_and_the_bottom_survives() {
    use ratatui::text::Line;
    let long = Line::from("x".repeat(50));
    let fitted = render::fit(
        vec![long.clone(), long.clone(), Line::from("consent (c)")],
        20,
        4,
    );
    assert_eq!(
        fitted.len(),
        2,
        "one 3-row line + the 1-row consent fit in 4"
    );
    assert_eq!(fitted.last().unwrap().to_string(), "consent (c)");

    let theme = App::test_fixture().theme;
    let c = CardState::new(
        plan(Verdict::Trusted, Some(SpawnChange::NoneToStrict)),
        Origin::Direct,
    );
    let wide = render::height(&c, theme, 200, 200, 100, 100);
    let narrow = render::height(&c, theme, 30, 200, 100, 100);
    assert!(
        narrow > wide,
        "narrow screen wraps to more rows ({narrow} vs {wide})"
    );
}

#[test]
fn the_card_takes_the_panel_slot_and_a_gate_outranks_it() {
    let mut app = App::test_fixture();
    app.ssh.card = Some(Box::new(CardState::new(
        plan(Verdict::Trusted, None),
        Origin::Direct,
    )));
    let (chooser, panel, chip, gated) =
        crate::cmd::agent::cli::ui::docked_rows_for_test(&app, 40, 3);
    assert!(panel > 3, "card gets rows");
    assert_eq!((chooser, chip, gated), (0, 0, false));
}
