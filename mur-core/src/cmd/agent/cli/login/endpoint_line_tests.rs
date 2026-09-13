//! `endpoint_line_tests`, moved out of `login.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: dedented one level, paths one level deeper.

use super::*;

/// A status line must not resolve the credential: on macOS a `keychain:`
/// ref pops an authorization prompt, and typing `/login` to look at a table
/// is not asking for one. `mur agent doctor` resolves deliberately, because
/// a human asked it to check.
#[test]
fn the_endpoint_line_names_the_credential_without_resolving_it() {
    let src = include_str!("../login.rs");
    let body = src
        .split("fn agent_endpoint_line")
        .nth(1)
        .expect("function must exist");
    let body = &body[..body.find("\n}\n").unwrap_or(body.len())];
    assert!(
        !body.contains("resolve_blocking"),
        "a status line must not resolve a secret"
    );
}

/// An unknown agent must produce nothing rather than an error banner: the
/// OAuth table above is still useful and this line is an addition to it.
#[test]
fn an_unknown_agent_adds_nothing() {
    assert_eq!(agent_endpoint_line("no-such-agent-xyz"), "");
}
