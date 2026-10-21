use super::*;

/// `--goal` is a run-scoped override for the guarded loop only: it parses
/// with `--loop` (also as one `--goal=` token, which is how the agent
/// runtime's `fleet_run` passes it) and is refused without it.
#[test]
fn fleet_run_goal_requires_loop() {
    let cli = Cli::try_parse_from(["mur", "fleet", "run", "coder", "--loop", "--goal=--ship it"])
        .unwrap();
    let Commands::Fleet {
        action:
            crate::cli::actions::FleetAction::Run {
                goal,
                loop_flag,
                job,
                ..
            },
    } = cli.command
    else {
        panic!("not `fleet run`");
    };
    assert!(loop_flag);
    assert_eq!(goal.as_deref(), Some("--ship it"));
    assert_eq!(job, None);

    assert!(Cli::try_parse_from(["mur", "fleet", "run", "coder", "--goal", "x"]).is_err());
}
