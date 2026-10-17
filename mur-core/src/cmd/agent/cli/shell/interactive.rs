//! Which `!commands` must own the terminal rather than run on piped stdio.
//!
//! The piped path (`run::spawn`) gives the child a null stdin so it can never
//! race the TUI for keystrokes. That is right for `cargo test` and wrong for a
//! command whose whole job is to wait for the person: it reads EOF at once.

/// Leading argv of commands that block on the user's own input. Matched on
/// whitespace-split tokens, so extra spacing or trailing flags still match.
///
/// `mur browser auth` waits for Enter after a human signs in, and asks which
/// engine to use when `--browser` is omitted; both reads need a real tty.
///
/// `mur deep-research secret` prompts for an API key without echo and refuses
/// to take it as an argument (so it never lands in shell history); with a
/// null stdin it exits with "no key on stdin".
const INTERACTIVE_PREFIXES: &[&[&str]] = &[
    &["mur", "browser", "auth"],
    &["mur", "deep-research", "secret"],
    // Confirms before overwriting paths changed since the promote.
    &["mur", "agent", "turn", "undo"],
    // Confirms before quarantining a damaged channel log.
    &["mur", "channel", "repair"],
];

/// Whether `cmd` must be run with the terminal handed over.
///
/// Deliberately conservative: only a plain leading match. A pipeline or a
/// compound command (`a && mur browser auth ...`) stays on the piped path —
/// guessing which segment wants the tty is how the TUI loses its terminal to
/// a command that never needed it.
pub fn needs_terminal(cmd: &str) -> bool {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    INTERACTIVE_PREFIXES
        .iter()
        .any(|prefix| tokens.len() >= prefix.len() && tokens[..prefix.len()] == **prefix)
}

/// `!mur fleet review …` and `!mur fleet review-resume …` are attended and
/// refuse a null stdin, yet a terminal handover would bypass murmur's own
/// review UI (modal HITL, footer, Esc pause). These map to the `/review` line
/// that does the same thing in place: (leading argv, slash line it becomes).
const REVIEW_REDIRECTS: &[(&[&str], &str)] = &[
    (&["mur", "fleet", "review"], "/review"),
    (&["mur", "fleet", "review-resume"], "/review resume"),
];

/// The `/review …` line equivalent to `cmd`, if `cmd` is an attended review
/// command (#1779), plus the `mur fleet <sub>` command that was typed
/// (`review` or `review-resume`) so the hint can quote it back. Arguments are
/// carried over verbatim — the person sees the line in the composer and
/// confirms it, so nothing is guessed silently.
pub fn review_redirect_named(cmd: &str) -> Option<(&'static str, String)> {
    let mut rest = cmd.trim_start();
    for (prefix, slash) in REVIEW_REDIRECTS {
        let mut r = rest;
        let matched = prefix.iter().all(|word| {
            let Some(after) = r.strip_prefix(word) else {
                return false;
            };
            if !(after.is_empty() || after.starts_with(char::is_whitespace)) {
                return false;
            }
            r = after.trim_start();
            true
        });
        if matched {
            rest = r.trim_end();
            let line = if rest.is_empty() {
                (*slash).to_string()
            } else {
                format!("{slash} {rest}")
            };
            return Some((prefix[prefix.len() - 1], line));
        }
    }
    None
}

/// The argv for a handover: the same shell `run::spawn` would use, so quoting
/// and expansion behave exactly as on the piped path.
pub fn handover_argv(cmd: &str) -> Vec<String> {
    #[cfg(unix)]
    let (shell, flag) = (
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        "-c",
    );
    #[cfg(windows)]
    let (shell, flag) = (
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd".into()),
        "/C",
    );
    vec![shell, flag.into(), cmd.into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review_redirect(cmd: &str) -> Option<String> {
        review_redirect_named(cmd).map(|(_, line)| line)
    }

    #[test]
    fn browser_auth_needs_the_terminal() {
        assert!(needs_terminal(
            "mur browser auth talent --url https://example.com/admin --browser chrome"
        ));
        assert!(needs_terminal(
            "  mur   browser  auth x --url https://e.com"
        ));
    }

    #[test]
    fn deep_research_secret_needs_the_terminal() {
        assert!(needs_terminal("mur deep-research secret --brave"));
        assert!(needs_terminal("mur agent turn undo mur t1"));
        assert!(needs_terminal("mur channel repair c1"));
        assert!(!needs_terminal("mur agent turn list mur"));
        assert!(!needs_terminal("mur deep-research \"some question\""));
        assert!(!needs_terminal("mur deep-research"));
    }

    #[test]
    fn ordinary_and_compound_commands_stay_piped() {
        assert!(!needs_terminal("mur browser status"));
        assert!(!needs_terminal("cargo test"));
        assert!(!needs_terminal("echo mur browser auth"));
        assert!(!needs_terminal(
            "true && mur browser auth x --url https://e.com"
        ));
        assert!(!needs_terminal(""));
    }

    #[test]
    fn attended_review_commands_map_to_the_slash_line() {
        assert_eq!(
            review_redirect("mur fleet review --main a --reviewer b noop task").as_deref(),
            Some("/review --main a --reviewer b noop task")
        );
        assert_eq!(
            review_redirect("  mur  fleet   review-resume review-x1 ").as_deref(),
            Some("/review resume review-x1")
        );
        assert_eq!(
            review_redirect("mur fleet review").as_deref(),
            Some("/review")
        );
    }

    #[test]
    fn the_redirect_names_the_command_that_was_typed() {
        let named = |c| review_redirect_named(c).map(|(sub, _)| sub);
        assert_eq!(named("mur fleet review --main a"), Some("review"));
        assert_eq!(named("mur fleet review-resume x"), Some("review-resume"));
        assert_eq!(named("mur fleet status x"), None);
    }

    #[test]
    fn other_fleet_commands_are_not_redirected() {
        assert_eq!(review_redirect("mur fleet reviews"), None);
        assert_eq!(
            review_redirect("mur fleet prune-reviews --older-than 30d"),
            None
        );
        assert_eq!(review_redirect("mur fleet status x"), None);
        assert_eq!(review_redirect("echo mur fleet review"), None);
        assert_eq!(review_redirect("true && mur fleet review --main a"), None);
    }

    #[test]
    fn handover_argv_runs_the_command_through_a_shell() {
        let argv = handover_argv("mur browser auth x --url 'https://e.com'");
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], "mur browser auth x --url 'https://e.com'");
    }
}
