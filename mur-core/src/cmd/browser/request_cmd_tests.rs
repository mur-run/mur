use chrono::Utc;
use mur_browser_request::{BrowserRequest, read_pending};

use super::*;
use crate::cmd::browser::request_card::{LoginAgain, ProfileRow};

fn req(site: &str, login: bool) -> PendingRequest {
    let t = tempfile::tempdir().unwrap();
    let s = Site::parse(site).unwrap();
    mur_browser_request::write(
        t.path(),
        &BrowserRequest::new(&s, login, "PR", Utc::now()).unwrap(),
    )
    .unwrap();
    read_pending(t.path(), Utc::now()).remove(0)
}

fn row(name: &str, offer: Offer) -> ProfileRow {
    ProfileRow {
        name: name.into(),
        domains: vec!["g.example".into()],
        shared_with: vec![],
        offer,
        session_elsewhere: None,
    }
}

fn login_card() -> Card {
    Card::Login {
        rows: vec![
            row("a", Offer::Offered),
            row(
                "b",
                Offer::ReplaceOnly {
                    with: "old".into(),
                    domains: vec!["g.example".into()],
                    drops: vec!["old".into(), "other".into()],
                    adds: vec![],
                },
            ),
            row(
                "c",
                Offer::Disabled {
                    why: "deny_hosts blocks g.example".into(),
                },
            ),
        ],
        login_again: LoginAgain {
            name: "g.example-3000".into(),
            exists: true,
        },
        login_offer: Offer::Offered,
        port: Some(3000),
    }
}

fn args<'a>() -> AnswerArgs<'a> {
    AnswerArgs {
        agent: "rev",
        site: "http://g.example:3000",
        profile: None,
        login: false,
        name: None,
        reauth: false,
        allow: false,
        replace: false,
        open_port: false,
        dismiss: false,
    }
}

#[test]
fn render_names_every_row_and_the_command_for_it() {
    let text = render("rev", &req("http://g.example:3000", true), &login_card()).join("\n");
    for want in [
        "rev asks for http://g.example:3000 (login needed): \"PR\"",
        "a: → mur browser answer rev http://g.example:3000 --profile a",
        "(drops old, other) → mur browser answer rev http://g.example:3000 --profile b --replace",
        "c: not available: deny_hosts blocks g.example",
        "--login --reauth   (replaces the saved login \"g.example-3000\"",
        "--open-port (opens port 3000 to ANY host",
        "--dismiss",
    ] {
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

/// #2461: the printed login command carries `--replace` when the login
/// only binds as a replacement, and names what it drops.
#[test]
fn render_a_replace_only_login_prints_replace() {
    let mut c = login_card();
    if let Card::Login {
        login_again,
        login_offer,
        ..
    } = &mut c
    {
        login_again.exists = false;
        *login_offer = Offer::ReplaceOnly {
            with: "vercel".into(),
            domains: vec!["github.com".into()],
            drops: vec!["vercel".into()],
            adds: vec![],
        };
    }
    let text = render("rev", &req("http://g.example:3000", true), &c).join("\n");
    assert!(
        text.contains(
            "--login --replace   (saves as \"g.example-3000\"; replaces the binding vercel"
        ),
        "{text}"
    );
}

#[test]
fn render_a_reach_card_always_says_allow() {
    let r = req("g.example", false);
    for (reach, want) in [
        (
            Reach::Allowed,
            "reachable already → mur browser answer rev https://g.example --allow",
        ),
        (
            Reach::NeedsHost {
                missing: vec!["g.example".into()],
            },
            "allows g.example →",
        ),
        (
            Reach::Unscoped {
                mode: "inherit".into(),
            },
            "network mode inherit",
        ),
        (
            Reach::Disabled {
                why: "network mode off is not host-scoped".into(),
            },
            "not available",
        ),
    ] {
        let text = render("rev", &r, &Card::Reach { reach, port: None }).join("\n");
        assert!(text.contains(want), "missing {want:?} in\n{text}");
    }
}

/// D11, council B5: `--profile` must be a row, and the row decides the rest.
#[test]
fn choose_takes_only_what_the_card_printed() {
    let c = login_card();
    let pick = |p: &'static str, replace: bool| {
        choose(
            &c,
            &AnswerArgs {
                profile: Some(p),
                replace,
                ..args()
            },
        )
    };
    assert!(matches!(pick("a", false), Ok(Choice::Use { ref profile, .. }) if profile == "a"));
    assert!(
        pick("typo", false)
            .unwrap_err()
            .contains("not on this card (rows: a, b, c)")
    );
    assert!(pick("b", false).unwrap_err().contains("--replace"));
    assert!(matches!(
        pick("b", true),
        Ok(Choice::Use { replace: true, .. })
    ));
    assert!(pick("c", false).unwrap_err().contains("deny_hosts"));
    assert!(pick("a", true).unwrap_err().contains("needs no --replace"));
}

/// D7, council N3: a bare answer never grants anything.
#[test]
fn choose_refuses_a_bare_answer() {
    assert!(
        choose(&login_card(), &args())
            .unwrap_err()
            .contains("needs a login")
    );
    let reach = Card::Reach {
        reach: Reach::Allowed,
        port: None,
    };
    assert!(choose(&reach, &args()).unwrap_err().contains("--allow"));
    assert_eq!(
        choose(
            &reach,
            &AnswerArgs {
                allow: true,
                ..args()
            }
        ),
        Ok(Choice::Allow { approved: vec![] })
    );
    let needs = Card::Reach {
        reach: Reach::NeedsHost {
            missing: vec!["g.example".into()],
        },
        port: None,
    };
    assert_eq!(
        choose(
            &needs,
            &AnswerArgs {
                allow: true,
                ..args()
            }
        ),
        Ok(Choice::Allow {
            approved: vec!["g.example".into()]
        })
    );
    assert!(
        choose(
            &reach,
            &AnswerArgs {
                profile: Some("a"),
                ..args()
            }
        )
        .is_err()
    );
    assert_eq!(
        choose(
            &login_card(),
            &AnswerArgs {
                dismiss: true,
                ..args()
            }
        ),
        Ok(Choice::Dismiss)
    );
    assert!(
        choose(
            &Card::NoLiveEntry,
            &AnswerArgs {
                allow: true,
                ..args()
            }
        )
        .is_err()
    );
}

#[test]
fn an_unknown_agent_is_an_error_not_an_empty_list() {
    let all = vec!["asker".to_string(), "Other".to_string()];
    assert_eq!(one_agent(&all, "ASKER").unwrap(), "asker");
    let err = one_agent(&all, "askr").unwrap_err().to_string();
    assert!(err.contains("no agent named \"askr\""), "{err}");
}
