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
    fn handover_argv_runs_the_command_through_a_shell() {
        let argv = handover_argv("mur browser auth x --url 'https://e.com'");
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], "mur browser auth x --url 'https://e.com'");
    }
}
