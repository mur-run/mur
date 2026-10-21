use super::*;

/// Every backtick-quoted `mur ...` command in `messages` must parse with the
/// real CLI, so a hint cannot drift from a renamed subcommand (#2045).
fn assert_hint_commands_parse(messages: &[String]) {
    let mut seen = 0;
    for msg in messages {
        for (i, span) in msg.split('`').enumerate() {
            if i % 2 == 0 || !span.starts_with("mur ") {
                continue;
            }
            seen += 1;
            let argv = shell_words::split(span).unwrap_or_else(|e| panic!("`{span}`: {e}"));
            if let Err(e) = Cli::try_parse_from(argv) {
                panic!("hint `{span}` in {msg:?} does not parse: {e}");
            }
        }
    }
    assert!(seen > 0, "no `mur ...` hint found in {messages:?}");
}

#[test]
fn reseal_hints_parse() {
    use crate::cmd::agent::reseal_summary;
    for matched in [true, false] {
        for cleared in [true, false] {
            for service in [true, false] {
                let lines = reseal_summary("carol", matched, cleared, service);
                if matched && (!cleared || service) {
                    continue; // these say nothing runnable
                }
                assert_hint_commands_parse(&lines);
            }
        }
    }
}

#[test]
fn rollback_hints_parse() {
    use crate::cmd::agent_history::rollback_summary;
    use crate::store::versioned::agent::{AgentRevision, RollbackOutcome};
    let out = RollbackOutcome {
        revision: AgentRevision {
            name: "carol".into(),
            version: 3,
            sha: "abc123".into(),
        },
        changed_entitlements: vec!["filesystem".into()],
        pin_advanced: false,
    };
    assert_hint_commands_parse(&rollback_summary("carol", 1, &out));
}
