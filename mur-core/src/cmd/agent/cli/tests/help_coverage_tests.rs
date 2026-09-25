//! `help_coverage_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.
//!
//! #940: `/open` worked, the chat footer advertised it, and `/help` did not
//! list it — so the one place a user goes to learn the command surface was
//! the one place it was missing. A hand-maintained list of "documented"
//! names (the original fix here) can catch a name that is present but
//! wrong; it can never catch a command the list simply forgot to include —
//! which is exactly what let `/login` go unlisted despite being fully
//! wired up. `help_name` below replaces that list with a `match` over
//! `SlashCmd` itself, with no wildcard arm: adding a new variant to
//! `SlashCmd` fails this test build until it is given an explicit line
//! here. Same lever as `#[expect]` over `#[allow]` — "someone must
//! remember" becomes "the build stops."

use super::super::app::{SlashCmd, parse_slash};
use super::super::help_text;

/// Canonical `/name` for a `SlashCmd` variant that must show up in
/// `/help`, or `None` for a variant that legitimately has none.
/// `Unknown` (the parser's not-a-command fallback) is the only such
/// variant today. No `_` arm on purpose: a new `SlashCmd` variant must
/// get an explicit answer here before this module compiles.
fn help_name(cmd: &SlashCmd) -> Option<&'static str> {
    match cmd {
        SlashCmd::Help => Some("help"),
        SlashCmd::Clear => Some("clear"),
        SlashCmd::Card => Some("card"),
        SlashCmd::Sessions => Some("sessions"),
        SlashCmd::Channels { .. } => Some("channels"),
        SlashCmd::Auto(_) => Some("auto"),
        SlashCmd::Verbose(_) => Some("verbose"),
        SlashCmd::Mcp(_) => Some("mcp"),
        SlashCmd::Skill(_) => Some("skill"),
        SlashCmd::Remember(_) => Some("remember"),
        SlashCmd::Memories => Some("memories"),
        SlashCmd::Forget(_) => Some("forget"),
        SlashCmd::Skin(_) => Some("skin"),
        SlashCmd::Panel(_) => Some("panel"),
        SlashCmd::Browser(_) => Some("browser"),
        SlashCmd::Open => Some("open"),
        SlashCmd::DeepResearch(_) => Some("deep-research"),
        SlashCmd::Search(_) => Some("search"),
        SlashCmd::Monitor(_) => Some("monitor"),
        SlashCmd::Model(_) => Some("model"),
        SlashCmd::Effort { .. } => Some("effort"),
        SlashCmd::Login(_) => Some("login"),
        SlashCmd::Secret { .. } => Some("secret"),
        SlashCmd::Quit => Some("quit"),
        SlashCmd::Unknown(_) => None,
    }
}

/// One concrete instance per `SlashCmd` variant, to drive the round-trip
/// check below.
///
/// **This list is hand-maintained and nothing forces it to be complete.**
/// `help_name`'s match is exhaustive over the enum, so a new variant must
/// be *named* — but a variant absent from this list is silently never
/// checked. `Effort` was missing here for its whole life, which is exactly
/// why it reached users absent from `/help` and from the menu. Add the new
/// variant here when you add one to `SlashCmd`.
fn one_of_each() -> Vec<SlashCmd> {
    vec![
        SlashCmd::Help,
        SlashCmd::Clear,
        SlashCmd::Card,
        SlashCmd::Sessions,
        SlashCmd::Channels {
            n: None,
            follow: false,
        },
        SlashCmd::Auto(None),
        SlashCmd::Verbose(None),
        SlashCmd::Mcp(vec![]),
        SlashCmd::Skill(vec![]),
        SlashCmd::Remember(vec![]),
        SlashCmd::Memories,
        SlashCmd::Forget(None),
        SlashCmd::Skin(None),
        SlashCmd::Panel(vec![]),
        SlashCmd::Browser(vec![]),
        SlashCmd::Open,
        SlashCmd::DeepResearch(vec![]),
        SlashCmd::Search(vec![]),
        SlashCmd::Monitor(vec![]),
        SlashCmd::Model(None),
        SlashCmd::Effort {
            level: None,
            save: false,
        },
        SlashCmd::Login(None),
        SlashCmd::Secret {
            key: None,
            delete: false,
        },
        SlashCmd::Quit,
        SlashCmd::Unknown("x".into()),
    ]
}

/// Three lists describe the same set of commands — `parse_slash`, `HELP`,
/// and the completion table — and nothing but this test ties them
/// together. `/effort` shipped in the parser while missing from both of
/// the others; that is the drift this exists to catch.
/// The composer hint and `/help` describe the same keys; the skin list in
/// `/help` is the one `/skin` accepts. Both drifted by hand once.
#[test]
fn help_matches_the_composer_hint_and_the_skin_list() {
    let help = help_text();
    for key in ["Enter", "Shift+Enter", "Ctrl+V", "Ctrl+O", "Ctrl+D"] {
        assert!(
            super::super::app::ENTER_HINT_FULL.contains(key) && help.contains(key),
            "{key} must appear in both the composer hint and /help"
        );
    }
    for skin in super::super::theme::SKIN_NAMES.split(", ") {
        assert!(help.contains(skin), "/help does not list skin {skin}");
    }
    assert!(
        !help.contains("dark|"),
        "`dark` is an alias, not a listed skin"
    );
    // One group per row, indented under its heading — the wall of text
    // this replaced had every command on one line.
    assert!(help.contains("commands\n  chat      /clear"), "{help}");
    assert!(help.contains("\n  settings  /model"), "{help}");
    assert!(help.contains("\nkeys        Enter send"), "{help}");
}

#[test]
fn every_command_is_parsed_documented_and_offered() {
    for cmd in one_of_each() {
        let Some(name) = help_name(&cmd) else {
            continue;
        };
        let parsed = parse_slash(&format!("/{name}"));
        assert!(
            !matches!(parsed, Some(SlashCmd::Unknown(_)) | None),
            "/{name} is in the documented list but the parser rejects it: {parsed:?}"
        );
        assert!(
            help_text().contains(&format!("/{name}")),
            "/{name} works but /help never mentions it"
        );
        assert!(
            super::super::complete::offers(name),
            "/{name} works but the completion menu never offers it"
        );
    }
}
