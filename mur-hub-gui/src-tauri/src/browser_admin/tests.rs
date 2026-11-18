use std::time::Duration;

use chrono::Utc;
use mur_browser_request::Site;
use mur_core::cmd::browser::request_card::front::{
    Card, LoginAgain, Offer, PendingRequest, ProfileRow, Shown,
};

use super::view::{BodyView, OfferView, card_view};
use super::{CARD_TTL, HeldCards, MAX_CARDS};

/// `PendingRequest` is `#[non_exhaustive]`: make one the way the tool does.
fn req() -> PendingRequest {
    let dir = tempfile::tempdir().unwrap();
    let site = Site::parse("https://git.example.com:8443").unwrap();
    let r = mur_browser_request::BrowserRequest::new(&site, true, "read the CI logs", Utc::now())
        .unwrap();
    mur_browser_request::write(dir.path(), &r).unwrap();
    mur_browser_request::read_pending(dir.path(), Utc::now())
        .pop()
        .unwrap()
}

fn shown() -> Shown {
    Shown::for_test(
        "qa",
        req(),
        Card::Login {
            rows: vec![ProfileRow {
                name: "work".into(),
                domains: vec!["git.example.com".into()],
                shared_with: vec![],
                offer: Offer::ReplaceOnly {
                    with: "work".into(),
                    domains: vec!["git.example.com".into()],
                    drops: vec!["old".into()],
                    adds: vec![],
                },
            }],
            login_again: LoginAgain {
                name: "git-example-com".into(),
                exists: false,
            },
            port: Some(8443),
        },
    )
}

#[test]
fn cards_are_capped_and_expire() {
    let cards = HeldCards::default();
    for _ in 0..MAX_CARDS + 1 {
        cards.put(shown());
    }
    assert_eq!(cards.len(), MAX_CARDS);
    let id = cards.put(shown());
    cards.age(&id, CARD_TTL + Duration::from_secs(1));
    let e = cards.begin(&id, false).unwrap_err();
    assert!(e.contains("open it again"), "{e}");
    assert!(
        cards.begin(&id, false).is_err(),
        "an expired card is dropped"
    );
}

#[test]
fn an_unknown_id_is_an_error() {
    let e = HeldCards::default().begin("nope", false).unwrap_err();
    assert!(e.contains("open it again"), "{e}");
}

#[test]
fn logged_in_needs_an_open_login() {
    let cards = HeldCards::default();
    let id = cards.put(shown());
    assert!(cards.logged_in(&id).is_err(), "no login open");
    assert!(cards.logged_in("nope").is_err(), "unknown id");
    let (_, rx) = cards.begin(&id, true).unwrap();
    let mut rx = rx.expect("a login gets a receiver");
    cards.logged_in(&id).unwrap();
    assert!(rx.try_recv().is_ok());
    assert!(cards.logged_in(&id).is_err(), "pressed once");
}

#[test]
fn closing_a_card_cancels_its_login() {
    let cards = HeldCards::default();
    let id = cards.put(shown());
    let (_, rx) = cards.begin(&id, true).unwrap();
    cards.close(&id);
    assert!(rx.unwrap().try_recv().is_err(), "sender dropped: cancelled");
}

#[test]
fn one_answer_at_a_time_and_a_kept_card_can_be_answered_again() {
    let cards = HeldCards::default();
    let id = cards.put(shown());
    cards.begin(&id, false).unwrap();
    assert!(cards.begin(&id, false).unwrap_err().contains("already"));
    cards.end(&id, true);
    cards.begin(&id, false).unwrap();
    cards.end(&id, false);
    assert!(cards.begin(&id, false).is_err(), "a done card is gone");
}

#[test]
fn the_view_keeps_replace_explicit_and_the_port_cli_only() {
    let v = card_view("id1", &shown());
    let BodyView::Login { rows, .. } = &v.body else {
        panic!("login body");
    };
    assert!(matches!(rows[0].offer, OfferView::ReplaceOnly { .. }));
    let line = v.port_line.expect("port step");
    assert!(
        line.contains("ANY host") && line.contains("--open-port"),
        "{line}"
    );
}
