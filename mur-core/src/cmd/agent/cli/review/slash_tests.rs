//! P3b-§3: `/review` typed in the composer reaches `review::handle` through
//! `submit` → `parse_slash` → `handle_slash` — the path a user actually takes.

use super::test_fixtures::{app_at, home, system_lines, tx};
use crate::cmd::agent::cli::turn::submit;
use crate::cmd::fleet::review::constants::{REVIEW_NO_PAUSED, REVIEW_USAGE_MURMUR};

/// Bare `/review` with nothing paused lists nothing and says so.
#[tokio::test]
async fn typed_bare_review_lists_paused_sessions() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    app.set_input("/review");

    submit(&mut app, &tx()).await;

    let text = system_lines(&app).join("\n");
    assert!(text.contains(REVIEW_NO_PAUSED), "{text}");
    assert!(!text.contains("unknown command"), "{text}");
    assert!(app.review.is_none());
}

/// AC-P3b-5 end to end: a refused line comes back into the composer whole.
#[tokio::test]
async fn typed_refused_review_puts_the_line_back() {
    let tmp = home();
    let mut app = app_at(tmp.path());
    let typed = "/review --main a";
    app.set_input(typed);

    submit(&mut app, &tx()).await;

    assert_eq!(app.input_text(), typed);
    let all: Vec<&str> = app.messages.iter().map(|m| m.text.as_str()).collect();
    assert!(
        all.iter().any(|t| t.contains(REVIEW_USAGE_MURMUR)),
        "{all:?}"
    );
    assert!(app.review.is_none());
}
