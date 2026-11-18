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
        session_elsewhere: None,
    }
}

fn login_card(rows: Vec<ProfileRow>, port: Option<u16>) -> State {
    login_card_with(rows, port, Offer::Offered)
}

fn login_card_with(rows: Vec<ProfileRow>, port: Option<u16>, login_offer: Offer) -> State {
    State::new(Shown::for_test(
        "qa",
        req(true),
        Card::Login {
            rows,
            login_again: LoginAgain {
                name: "git-example".into(),
                exists: false,
            },
            login_offer,
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
    // The usable login line is the default; put the cursor on the row.
    assert_eq!(c.selected, c.login_at());
    c.selected = 0;
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

fn replaces(drops: &[&str]) -> Offer {
    Offer::ReplaceOnly {
        with: "vercel".into(),
        domains: vec![],
        drops: drops.iter().map(|s| s.to_string()).collect(),
        adds: vec![],
    }
}

/// #2461 bug 1: a login that only binds as a replacement is not refused
/// after `l`; `l` asks for `r`, and `r` sends `replace: true`.
#[test]
fn a_replace_only_login_takes_l_then_r() {
    let mut c = login_card_with(
        vec![row("vercel", Offer::AlreadyBound)],
        None,
        replaces(&["vercel"]),
    );
    assert_eq!(c.on_key(k(KeyCode::Char('l'))), KeyAction::Nothing);
    assert_eq!(c.selected, c.login_at());
    let note = c.note.clone().unwrap();
    assert!(
        note.contains("replaces vercel") && note.contains("press r"),
        "{note}"
    );
    assert_eq!(
        c.on_key(k(KeyCode::Char('r'))),
        KeyAction::Settle(Pick::Login {
            name: None,
            reauth: false,
            replace: true
        })
    );
    let s = text(&c);
    assert!(
        s.contains("only by replacing vercel — press r to log in"),
        "{s}"
    );
}

#[test]
fn a_login_that_cannot_bind_opens_nothing() {
    let mut c = login_card_with(vec![], None, Offer::Disabled { why: "off".into() });
    assert_eq!(c.on_key(k(KeyCode::Char('l'))), KeyAction::Nothing);
    assert!(c.note.as_deref().unwrap().contains("not available: off"));
    assert_eq!(c.on_key(k(KeyCode::Char('r'))), KeyAction::Nothing);
}

/// #2461 bug 2: the card does not default to a row whose session is for
/// another site; the login line is the default and takes Enter.
#[test]
fn the_default_skips_a_row_with_no_session_for_the_site() {
    let mut vercel = row("vercel", Offer::AlreadyBound);
    vercel.session_elsewhere = Some("vercel.com".into());
    let mut c = login_card(vec![vercel.clone()], None);
    assert_eq!(c.selected, c.login_at());
    assert!(text(&c).contains("logged in at vercel.com — may have no session for git.example.com"));
    assert_eq!(
        c.on_key(k(KeyCode::Enter)),
        KeyAction::Settle(Pick::Login {
            name: None,
            reauth: false,
            replace: false
        })
    );
    // A row with a session for the site stays the default.
    let c = login_card(vec![vercel, row("gh", Offer::Offered)], None);
    assert_eq!(c.selected, 1);
    // The flagged row can still be picked on purpose.
    let mut c = login_card(
        vec![{
            let mut r = row("vercel", Offer::Offered);
            r.session_elsewhere = Some("vercel.com".into());
            r
        }],
        None,
    );
    c.on_key(k(KeyCode::Up));
    assert_eq!(
        c.on_key(k(KeyCode::Enter)),
        KeyAction::Settle(Pick::Row {
            name: "vercel".into(),
            replace: false
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
