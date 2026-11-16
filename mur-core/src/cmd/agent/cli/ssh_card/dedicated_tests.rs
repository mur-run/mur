//! The card's dedicated-key rows (slice 6c): the extra row per `Dedicated`
//! state, the install toggle, and the held and done steps.

use crossterm::event::KeyCode;

use super::clip::osc52;
use super::render;
use super::state::{CardState, ExtraRow, KeyAction, Origin, Step};
use super::tests::{fp, key, plan, press};
use super::{CardMsg, close_written_note, on_key, on_msg};
use crate::cmd::agent::cli::theme;
use crate::cmd::agent::ssh::setup::plan::{Dedicated, DedicatedOff};
use crate::cmd::agent::ssh::setup::{Approval, Verdict};

const LINE: &str = "ssh-ed25519 AAAAdedicated mur-agent-qa";

fn card(dedicated: Dedicated) -> CardState {
    let mut p = plan(Verdict::Trusted, None);
    p.dedicated = dedicated;
    CardState::new(p, Origin::Direct)
}

/// A stored, loaded dedicated key `D`, chosen.
fn with_dedicated() -> CardState {
    let mut c = card(Dedicated::Stored {
        fingerprint: fp('D'),
        public_line: LINE.into(),
    });
    c.plan.keys_loaded.push(key('D', None));
    c.key_idx = c
        .keys()
        .iter()
        .position(|(k, _)| k.fingerprint == fp('D'))
        .unwrap();
    c
}

fn text(c: &CardState) -> String {
    render::lines(c, &theme::ANSI)
        .iter()
        .map(|l| l.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn approved(c: &mut CardState) -> Approval {
    c.step = Step::Summary;
    match press(c, KeyCode::Enter) {
        KeyAction::Approve(a) => a,
        _ => panic!("not approved: {:?}", c.note),
    }
}

#[test]
fn the_extra_row_follows_the_dedicated_state() {
    assert_eq!(card(Dedicated::Offer).extra_row(), Some(ExtraRow::Generate));
    let off = card(Dedicated::Off(DedicatedOff::SwitchOff));
    assert!(matches!(off.extra_row(), Some(ExtraRow::Off(why)) if why.contains("generate_key")));
    assert_eq!(with_dedicated().extra_row(), None, "stored and loaded");
    // Not in the user's ssh-agent, yet listed first and ready (K8).
    let mut stored = card(Dedicated::Stored {
        fingerprint: fp('D'),
        public_line: LINE.into(),
    });
    let (loaded, unloaded) = (
        stored.plan.keys_loaded.clone(),
        stored.plan.keys_unloaded.clone(),
    );
    stored.refresh_keys(loaded, unloaded);
    assert_eq!(stored.extra_row(), None);
    let keys = stored.keys();
    assert_eq!(
        keys.first().map(|(k, l)| (k.fingerprint.clone(), *l)),
        Some((fp('D'), true)),
        "listed first, ready"
    );
    assert!(text(&card(Dedicated::Offer)).contains("generate a dedicated key"));
}

/// The live path: the plan learns its keys through `set_keys` after the
/// dedicated key is known, so a fresh card opens with `D` already chosen.
#[test]
fn a_fresh_card_opens_on_the_stored_dedicated_key() {
    let mut p = plan(Verdict::Trusted, None);
    p.dedicated = Dedicated::Stored {
        fingerprint: fp('D'),
        public_line: LINE.into(),
    };
    let (loaded, unloaded) = (p.keys_loaded.clone(), p.keys_unloaded.clone());
    assert!(
        !loaded.is_empty(),
        "D must outrank a key that is already loaded"
    );
    p.set_keys(loaded, unloaded);
    let c = CardState::new(p, Origin::Direct);
    assert_eq!(
        c.chosen_key().map(|(k, l)| (k.fingerprint.clone(), l)),
        Some((fp('D'), true)),
        "chosen on open, ready"
    );
}

#[test]
fn enter_on_the_generate_row_asks_for_a_key() {
    let mut c = card(Dedicated::Offer);
    let n = c.keys().len();
    for _ in 0..n {
        press(&mut c, KeyCode::Down);
    }
    assert_eq!(c.key_idx, n, "↓ reaches the extra row");
    assert!(matches!(press(&mut c, KeyCode::Enter), KeyAction::Generate));
    assert_eq!(c.step, Step::Key, "generating grants nothing");
}

#[test]
fn an_off_row_is_shown_but_not_selectable() {
    let mut c = card(Dedicated::Off(DedicatedOff::NotSealedEveryRun));
    c.key_idx = c.keys().len();
    assert!(matches!(press(&mut c, KeyCode::Enter), KeyAction::Nothing));
    assert!(c.note.as_deref().unwrap().contains("sandboxed"));
    assert!(text(&c).contains("dedicated key unavailable"));
}

#[test]
fn a_generated_key_becomes_the_selection() {
    let mut c = card(Dedicated::Offer);
    let mut loaded = c.plan.keys_loaded.clone();
    loaded.push(key('D', None));
    let stored = Dedicated::Stored {
        fingerprint: fp('D'),
        public_line: LINE.into(),
    };
    c.generated(stored, loaded, vec![], &fp('D'));
    assert_eq!(c.chosen_key().unwrap().0.fingerprint, fp('D'));
    assert_eq!(c.extra_row(), None);
    assert_eq!(c.dedicated_line(), Some(LINE));
}

#[test]
fn the_install_toggle_needs_a_user_and_the_dedicated_key() {
    let mut c = with_dedicated();
    c.step = Step::Summary;
    c.user.clear();
    press(&mut c, KeyCode::Char('i'));
    assert!(!c.install);
    assert_eq!(c.note.as_deref(), Some("set a user first (u)"));
    assert!(text(&c).contains("set a user first (u)"));

    c.user = "git".into();
    press(&mut c, KeyCode::Char('i'));
    assert!(c.install);
    let t = text(&c);
    assert!(
        t.contains("[x] install the public key on git@gh.example"),
        "{t}"
    );
    assert!(t.contains(LINE), "the exact line is on the summary");

    let mut other = card(Dedicated::Offer);
    other.step = Step::Summary;
    press(&mut other, KeyCode::Char('i'));
    assert!(!other.install, "no toggle for one of the user's own keys");
}

#[test]
fn the_approval_carries_the_install() {
    let mut c = with_dedicated();
    assert!(approved(&mut c).install.is_none(), "off by default");
    let mut c = with_dedicated();
    c.install = true;
    let t = approved(&mut c).install.expect("install target");
    assert_eq!(t.user, "git");
    assert_eq!(t.host, c.plan.host);
}

#[test]
fn a_held_restart_offers_retry_copy_and_restart_anyway() {
    let mut c = with_dedicated();
    c.install = true;
    let a = approved(&mut c);
    assert!(c.after_apply(a, true), "the card stays open");
    assert_eq!(c.step, Step::Held);
    assert!(text(&c).contains("key not installed"));
    assert!(matches!(
        press(&mut c, KeyCode::Char('r')),
        KeyAction::RetryInstall(_)
    ));
    assert!(matches!(press(&mut c, KeyCode::Char('y')), KeyAction::Copy(l) if l == LINE));
    assert!(matches!(
        press(&mut c, KeyCode::Enter),
        KeyAction::Finish(_)
    ));
    assert!(matches!(press(&mut c, KeyCode::Left), KeyAction::Nothing));
    assert_eq!(c.step, Step::Held, "no way back to a step already written");
}

#[test]
fn no_install_with_the_dedicated_key_shows_the_line_and_the_advisory_test() {
    let mut c = with_dedicated();
    let a = approved(&mut c);
    assert!(c.after_apply(a, false));
    assert_eq!(c.step, Step::Done);
    assert!(text(&c).contains(LINE));
    match press(&mut c, KeyCode::Char('t')) {
        KeyAction::Advisory { user, host, .. } => {
            assert_eq!(user, "git");
            assert_eq!(host, c.plan.host);
        }
        _ => panic!("t is the advisory test"),
    }
    assert!(matches!(
        press(&mut c, KeyCode::Char('r')),
        KeyAction::Nothing
    ));
}

#[test]
fn any_other_key_closes_the_card_after_apply() {
    let mut c = CardState::new(plan(Verdict::Trusted, None), Origin::Direct);
    let a = approved(&mut c);
    assert!(!c.after_apply(a, false));
}

#[test]
fn closing_after_the_write_never_says_nothing_changed() {
    let mut c = with_dedicated();
    c.install = true;
    let a = approved(&mut c);
    assert!(c.after_apply(a, true));
    assert!(matches!(
        press(&mut c, KeyCode::Esc),
        KeyAction::CloseWritten { held: true }
    ));
    let held = close_written_note("qa", true);
    assert!(held.contains("grant is written"), "{held}");
    assert!(held.contains("mur agent restart qa"), "{held}");
    assert!(!held.contains("nothing was changed"), "{held}");

    let mut c = with_dedicated();
    let a = approved(&mut c);
    assert!(c.after_apply(a, false));
    assert!(matches!(
        press(&mut c, KeyCode::Esc),
        KeyAction::CloseWritten { held: false }
    ));
    assert!(matches!(
        press(&mut c, KeyCode::Enter),
        KeyAction::CloseWritten { held: false }
    ));
    let done = close_written_note("qa", false);
    assert!(done.contains("grant is written"), "{done}");
    assert!(!done.contains("nothing was changed"), "{done}");
}

#[test]
fn esc_on_the_held_step_tells_the_transcript_the_grant_is_written() {
    let mut app = crate::cmd::agent::cli::app::App::test_fixture();
    let mut c = with_dedicated();
    c.install = true;
    let a = approved(&mut c);
    assert!(c.after_apply(a, true));
    app.ssh.card = Some(Box::new(c));
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    on_key(
        &mut app,
        crossterm::event::KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE),
        &tx,
    );
    assert!(app.ssh.card.is_none());
    let last = &app.messages.last().expect("a notice").text;
    assert!(last.contains("not restarted"), "{last}");
    assert!(!last.contains("nothing was changed"), "{last}");
}

#[test]
fn a_crashed_job_gives_the_held_card_back() {
    let mut app = crate::cmd::agent::cli::app::App::test_fixture();
    let mut c = with_dedicated();
    c.install = true;
    let a = approved(&mut c);
    assert!(c.after_apply(a, true));
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    app.ssh.busy = true;
    on_msg(
        &mut app,
        CardMsg::Crashed {
            error: "task failed: boom".into(),
            card: Some(Box::new(c)),
        },
        &tx,
    );
    assert!(!app.ssh.busy);
    assert_eq!(app.ssh.card.as_ref().map(|c| c.step), Some(Step::Held));
    assert!(app.messages.last().unwrap().text.contains("boom"));
}

#[test]
fn osc52_wraps_the_line_in_base64() {
    assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
}
