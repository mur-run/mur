//! The browser card over hand-made cards: renders per offer, keys, and the
//! login gate. Nothing here settles against real entries.

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mur_browser_request::Site;

use super::render::{lines, offer_text};
use super::state::{KeyAction, Phase, State};
use crate::cmd::browser::request_card::front::{
    Card, LoginAgain, Offer, PendingRequest, Pick, ProfileRow, Reach, Shown, Status,
};

fn k(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

/// `PendingRequest` is `#[non_exhaustive]`: make one the way the tool does.
fn req(login: bool) -> PendingRequest {
    let dir = tempfile::tempdir().unwrap();
    let site = Site::parse("https://git.example.com").unwrap();
    let r = mur_browser_request::BrowserRequest::new(&site, login, "read the CI logs", Utc::now())
        .unwrap();
    mur_browser_request::write(dir.path(), &r).unwrap();
    mur_browser_request::read_pending(dir.path(), Utc::now())
        .pop()
        .unwrap()
}

fn row(name: &str, offer: Offer) -> ProfileRow {
    ProfileRow {
        name: name.into(),
        domains: vec!["git.example.com".into()],
        shared_with: vec![],
        offer,
    }
}

fn login_card(rows: Vec<ProfileRow>, port: Option<u16>) -> State {
    State::new(Shown::for_test(
        "qa",
        req(true),
        Card::Login {
            rows,
            login_again: LoginAgain {
                name: "git-example".into(),
                exists: false,
            },
            port,
        },
    ))
}

fn text(c: &State) -> String {
    let t = crate::cmd::agent::cli::app::App::test_fixture().theme;
    lines(c, t)
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn every_offer_renders_its_own_line() {
    let cases = [
        (Offer::AlreadyBound, "already in use"),
        (Offer::Offered, "use this login"),
        (
            Offer::AddsHosts {
                missing: vec!["cdn.example.com".into()],
            },
            "also adds cdn.example.com",
        ),
        (
            Offer::ReplaceOnly {
                with: "work".into(),
                domains: vec![],
                drops: vec!["old".into()],
                adds: vec![],
            },
            "only by replacing old",
        ),
        (
            Offer::Disabled {
                why: "deny_hosts blocks it".into(),
            },
            "not available: deny_hosts",
        ),
    ];
    for (offer, want) in cases {
        assert!(offer_text(&offer).contains(want), "{offer:?}");
        let c = login_card(vec![row("work", offer)], None);
        assert!(text(&c).contains(want));
    }
}

#[test]
fn shared_with_and_port_block_render() {
    let mut r = row("work", Offer::Offered);
    r.shared_with = vec!["ops".into()];
    let c = login_card(vec![r], Some(8443));
    let s = text(&c);
    assert!(s.contains("shared with ops"));
    assert!(s.contains("ANY host"), "{s}");
    assert!(s.contains("--open-port"), "{s}");
}

#[test]
fn enter_on_a_disabled_row_picks_nothing() {
    let mut c = login_card(
        vec![row("blocked", Offer::Disabled { why: "off".into() })],
        None,
    );
    assert_eq!(c.on_key(k(KeyCode::Enter)), KeyAction::Nothing);
    assert_eq!(c.phase, Phase::Choosing);
    assert!(c.note.is_some());
}

#[test]
fn arrows_skip_disabled_rows() {
    let mut c = login_card(
        vec![
            row("a", Offer::Offered),
            row("x", Offer::Disabled { why: "off".into() }),
            row("b", Offer::Offered),
        ],
        None,
    );
    assert_eq!(c.selected, 0);
    c.on_key(k(KeyCode::Down));
    assert_eq!(c.selected, 2);
    assert_eq!(
        c.on_key(k(KeyCode::Enter)),
        KeyAction::Settle(Pick::Row {
            name: "b".into(),
            replace: false
        })
    );
}

#[test]
fn a_replace_row_needs_r_not_enter() {
    let offer = Offer::ReplaceOnly {
        with: "work".into(),
        domains: vec![],
        drops: vec!["old".into()],
        adds: vec![],
    };
    let mut c = login_card(vec![row("work", offer)], None);
    assert_eq!(c.on_key(k(KeyCode::Enter)), KeyAction::Nothing);
    assert_eq!(
        c.on_key(k(KeyCode::Char('r'))),
        KeyAction::Settle(Pick::Row {
            name: "work".into(),
            replace: true
        })
    );
}

#[test]
fn logging_in_takes_only_enter_and_esc() {
    let mut c = login_card(vec![], None);
    assert_eq!(
        c.on_key(k(KeyCode::Char('l'))),
        KeyAction::Settle(Pick::Login {
            name: None,
            reauth: false,
            replace: false
        })
    );
    c.phase = Phase::LoggingIn;
    assert_eq!(c.on_key(k(KeyCode::Char('d'))), KeyAction::Nothing);
    assert_eq!(c.on_key(k(KeyCode::Esc)), KeyAction::CancelLogin);
    assert_eq!(c.on_key(k(KeyCode::Enter)), KeyAction::LoggedIn);
}

#[test]
fn reach_card_allows_unless_disabled() {
    let mk = |reach| {
        State::new(Shown::for_test(
            "qa",
            req(false),
            Card::Reach { reach, port: None },
        ))
    };
    let mut ok = mk(Reach::NeedsHost {
        missing: vec!["git.example.com".into()],
    });
    assert_eq!(
        ok.on_key(k(KeyCode::Char('a'))),
        KeyAction::Settle(Pick::Allow)
    );
    let mut no = mk(Reach::Disabled {
        why: "browser is off".into(),
    });
    assert_eq!(no.on_key(k(KeyCode::Char('a'))), KeyAction::Nothing);
    assert_eq!(
        no.on_key(k(KeyCode::Char('d'))),
        KeyAction::Settle(Pick::Dismiss)
    );
}

#[test]
fn saved_shows_the_d4_label() {
    let mut c = login_card(vec![], None);
    c.phase = Phase::Saved {
        profile: "git-example".into(),
        status: Status::SavedNotTried,
    };
    assert!(text(&c).contains("saved, not yet tried"));
}

#[test]
fn cancel_login_drops_the_sender() {
    let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
    let mut slot = super::BrowserSlot {
        logged_in: Some(tx),
        ..Default::default()
    };
    slot.logged_in = None; // what CancelLogin does
    assert!(rx.try_recv().is_err());
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}

#[test]
fn ssh_card_outranks_the_browser_card() {
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let mut app = crate::cmd::agent::cli::app::App::test_fixture();
    app.browser.card = Some(Box::new(login_card(vec![row("a", Offer::Offered)], None)));
    // Browser card alone: it owns the key and Esc closes it.
    assert!(crate::cmd::agent::cli::cards::on_key(
        &mut app,
        k(KeyCode::Esc),
        false,
        &tx
    ));
    assert!(app.browser.card.is_none());
    // Ctrl combos still reach the composer.
    app.browser.card = Some(Box::new(login_card(vec![], None)));
    assert!(!crate::cmd::agent::cli::cards::on_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        true,
        &tx
    ));
}

#[test]
fn browser_requests_with_nothing_pending_says_so() {
    let (tx, _rx) = tokio::sync::mpsc::channel(4);
    let mut app = crate::cmd::agent::cli::app::App::test_fixture();
    let home = tempfile::tempdir().unwrap();
    app.home = home.path().to_path_buf();
    super::command(&mut app, &[], &tx);
    assert!(app.browser.card.is_none() && !app.browser.busy);
}
