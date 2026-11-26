//! `mur` status queries: read-only inspection commands that must not compete
//! with real work for a scarce resource (the runtime's background-job cap).
//!
//! A fixed noun + verb allowlist, same shape as the `gh` lane in the parent
//! module. Anything not provably a one-shot read returns `false`.

/// `(noun, verbs)` — each verb only inspects state and exits.
const STATUS_QUERIES: &[(&str, &[&str])] = &[
    ("fleet", &["status", "list", "show"]),
    ("agent", &["status", "list", "card"]),
    ("model", &["list", "show", "doctor"]),
    ("monitor", &["list", "show"]),
];

/// The one flag a status query may carry. Any other `-…` word could turn it
/// into a follower, a writer, or something that prompts for stdin.
const ALLOWED_FLAG: &str = "--json";

/// Is `cmd` a single `mur <noun> <verb> [name] [--json]` status query?
pub fn is_mur_status_query(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.contains(super::SHELL_META) {
        return false;
    }
    let mut words = cmd.split_whitespace();
    if words.next() != Some("mur") {
        return false;
    }
    let (Some(noun), Some(verb)) = (words.next(), words.next()) else {
        return false;
    };
    let listed = STATUS_QUERIES
        .iter()
        .any(|(n, verbs)| *n == noun && verbs.contains(&verb));
    listed
        && words.all(|w| {
            if w.starts_with('-') {
                w == ALLOWED_FLAG
            } else {
                // A name, not a quoted or globbed expression.
                !w.contains(['\'', '"', '*', '?', '[', '~'])
            }
        })
}

#[cfg(test)]
mod tests {
    use super::is_mur_status_query;

    #[test]
    fn plain_status_queries_pass() {
        for ok in [
            "mur fleet status",
            "mur fleet status deep-research",
            "mur fleet list --json",
            "  mur agent status mur  ",
            "mur model doctor",
            "mur monitor show m-1",
        ] {
            assert!(is_mur_status_query(ok), "{ok}");
        }
    }

    #[test]
    fn writers_followers_and_chains_do_not() {
        for no in [
            "mur",
            "mur fleet",
            "mur fleet run x",
            "mur fleet stop x",
            "mur agent send mur hi",
            "mur monitor cancel m-1",
            "mur fleet status --watch",
            "mur fleet status x; rm -rf /",
            "mur fleet status | cat",
            "mur fleet status $(whoami)",
            "mur fleet status > out",
            "mur fleet status \"x\"",
            "murx fleet status",
            "env mur fleet status",
            "cargo test",
        ] {
            assert!(!is_mur_status_query(no), "{no}");
        }
    }
}
