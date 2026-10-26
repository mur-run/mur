use super::*;

/// #1917: the legacy-home recovery message tells the user to paste a
/// command. If it drifted from the real CLI (a renamed flag, bad
/// quoting) they would hit a clap error while already recovering from a
/// failure. Split the command out of the real message the way a shell
/// would, then hand it to the real parser. The repo paths cover a plain
/// one, one with spaces (`/Volumes/My Drive/...`) and one with a quote.
/// Lives here because `Cli` is in the bin crate, which the library's own
/// tests cannot see.
#[test]
fn serena_legacy_home_recovery_command_parses() {
    use clap::Parser;
    use mur_agent_runtime::mcp::serena::{serena_paths, verify_entries};
    use mur_common::agent::{McpServerEntry, McpServerKind};

    for dir in ["repo", "My Drive/repo", "it's here/repo"] {
        let tmp = tempfile::tempdir().unwrap();
        let agent_home = tmp.path().join("agents/carol");
        let repo = tmp.path().join(dir);
        std::fs::create_dir_all(&repo).unwrap();
        // Only the older shared home exists: the state the hint is for.
        std::fs::create_dir_all(serena_paths(&agent_home).home).unwrap();
        let entry = McpServerEntry {
            name: "code-nav".into(),
            command: "serena".into(),
            kind: Some(McpServerKind::Serena),
            project: Some(repo.clone()),
            ..Default::default()
        };
        // #2136: entry-scoped, so the hint comes back in the skip list.
        let msg = verify_entries(&[entry], &agent_home)
            .unwrap()
            .pop()
            .expect("legacy home must be reported")
            .to_string();

        // The command is the last backtick-quoted span; the path inside
        // it may itself hold a backtick-free quote, so cut on the span.
        let end = msg.rfind('`').unwrap();
        let start = msg[..end]
            .rfind("`mur ")
            .unwrap_or_else(|| panic!("no command: {msg}"));
        let cmd = &msg[start + 1..end];
        let argv = shell_words::split(cmd).unwrap_or_else(|e| panic!("`{cmd}`: {e}"));
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("`{cmd}`: {e}"));
        let Commands::CodeNav {
            action:
                CodeNavAction::Setup {
                    agent,
                    with_serena,
                    project,
                    entry,
                    ..
                },
        } = cli.command
        else {
            panic!("`{cmd}` is not `code-nav setup`");
        };
        assert_eq!(agent.as_deref(), Some("carol"));
        assert!(with_serena);
        assert_eq!(entry, "code-nav");
        assert_eq!(project, Some(repo), "`{cmd}`");
    }
}
