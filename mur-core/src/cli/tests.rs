use super::*;

#[test]
fn cli_parses_fleet_create() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "mur",
        "fleet",
        "create",
        "dev",
        "--members",
        "pm,qa",
        "--goal",
        "ship",
    ])
    .unwrap();
    match cli.command {
        Commands::Fleet {
            action:
                crate::cli::actions::FleetAction::Create {
                    name,
                    members,
                    goal,
                    ..
                },
        } => {
            assert_eq!(name, "dev");
            assert_eq!(members, vec!["pm".to_string(), "qa".to_string()]);
            assert_eq!(goal.as_deref(), Some("ship"));
        }
        _ => panic!("expected fleet create"),
    }
}

#[test]
fn cli_parses_official_install() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["mur", "official", "install", "fleets/deep-research"]).unwrap();
    match cli.command {
        Commands::Official {
            action:
                crate::cli::actions::OfficialAction::Install {
                    id,
                    model_policy,
                    model_ref,
                    fallback,
                },
        } => {
            assert_eq!(id, "fleets/deep-research");
            assert!(model_policy.is_none());
            assert!(model_ref.is_none());
            assert!(fallback.is_empty());
        }
        _ => panic!("expected official install"),
    }
}

#[test]
fn cli_parses_official_model_selection_flags() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "mur",
        "official",
        "install",
        "agents/orchestrator",
        "--model-ref",
        "primary",
        "--fallback",
        "fallback-one",
        "--fallback",
        "fallback-two",
    ])
    .unwrap();
    match cli.command {
        Commands::Official {
            action:
                crate::cli::actions::OfficialAction::Install {
                    model_ref,
                    fallback,
                    ..
                },
        } => {
            assert_eq!(model_ref.as_deref(), Some("primary"));
            assert_eq!(fallback, ["fallback-one", "fallback-two"]);
        }
        _ => panic!("expected official install"),
    }

    for policy in ["capability-first", "cost-first", "privacy-first"] {
        assert!(
            Cli::try_parse_from([
                "mur",
                "official",
                "install",
                "agents/orchestrator",
                "--model-policy",
                policy,
            ])
            .is_ok(),
            "policy {policy} should parse"
        );
    }
}

#[test]
fn cli_browser_doctor_parses_with_and_without_live() {
    use crate::cli::actions::BrowserAction;
    use clap::Parser;
    for (argv, want) in [
        (&["mur", "browser", "doctor"][..], false),
        (&["mur", "browser", "doctor", "--live"][..], true),
    ] {
        match Cli::try_parse_from(argv).unwrap().command {
            Commands::Browser {
                action: BrowserAction::Doctor { live },
            } => assert_eq!(live, want, "{argv:?}"),
            _ => panic!("expected browser doctor for {argv:?}"),
        }
    }
}

#[test]
fn cli_browser_replay_max_heal_ratio_is_bounded_and_defaults() {
    use crate::cli::actions::BrowserAction;
    use clap::Parser;
    let parse = |extra: &[&str]| {
        let mut argv = vec!["mur", "browser", "replay", "checkout"];
        argv.extend_from_slice(extra);
        Cli::try_parse_from(argv)
    };
    for bad in ["1.5", "-0.1", "NaN", "abc"] {
        assert!(parse(&["--max-heal-ratio", bad]).is_err(), "{bad}");
    }
    for (extra, want) in [
        (&[][..], mur_browser::heal::DEFAULT_HEAL_RATIO),
        (&["--max-heal-ratio", "0.5"][..], 0.5),
        (&["--max-heal-ratio", "0"][..], 0.0),
        (&["--max-heal-ratio", "1"][..], 1.0),
    ] {
        match parse(extra).unwrap().command {
            Commands::Browser {
                action: BrowserAction::Replay { max_heal_ratio, .. },
            } => assert_eq!(max_heal_ratio, want, "{extra:?}"),
            _ => panic!("expected browser replay for {extra:?}"),
        }
    }
}

/// Bare `setup` must stay the asking form: `yes` defaults to false, so
/// the consent prompts are only skipped when the flag is actually typed.
#[test]
fn cli_browser_setup_does_not_consent_by_default() {
    use crate::cli::actions::BrowserAction;
    use clap::Parser;
    assert!(matches!(
        Cli::try_parse_from(["mur", "browser", "setup"])
            .unwrap()
            .command,
        Commands::Browser {
            action: BrowserAction::Setup {
                agent: None,
                yes: false
            }
        }
    ));
}

/// `--yes` exists so setup is usable where there is no TTY to prompt on
/// (inside murmur). It replaces the typed `yes`; it does not widen what
/// setup may do, and it is never the default (see the test above).
#[test]
fn cli_browser_setup_takes_yes() {
    use crate::cli::actions::BrowserAction;
    use clap::Parser;
    assert!(matches!(
        Cli::try_parse_from(["mur", "browser", "setup", "--yes"])
            .unwrap()
            .command,
        Commands::Browser {
            action: BrowserAction::Setup { yes: true, .. }
        }
    ));
}

/// The permission step grants to a named agent; `--agent` must reach it
/// rather than being swallowed as an unknown flag.
#[test]
fn cli_browser_setup_takes_an_agent() {
    use crate::cli::actions::BrowserAction;
    use clap::Parser;
    let parsed = Cli::try_parse_from(["mur", "browser", "setup", "--agent", "mur"]).unwrap();
    let Commands::Browser {
        action: BrowserAction::Setup { agent, .. },
    } = parsed.command
    else {
        panic!("expected browser setup");
    };
    assert_eq!(agent.as_deref(), Some("mur"));
}

/// `mur deep-research status` must open the status panel, never be
/// swallowed as a one-word research question and dispatched to the
/// fleet (which is what happened before the `Status` subcommand existed).
#[test]
fn cli_deep_research_status_is_a_subcommand_not_a_question() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["mur", "deep-research", "status"]).unwrap();
    match cli.command {
        Commands::DeepResearch {
            action: Some(crate::cli::actions::DeepResearchAction::Status),
            question: None,
            ..
        } => {}
        Commands::DeepResearch { question, .. } => {
            panic!("`status` was parsed as a research question: {question:?}")
        }
        _ => panic!("expected deep-research"),
    }

    // A real question still routes to the ask path.
    let cli = Cli::try_parse_from(["mur", "deep-research", "how do starlings sing"]).unwrap();
    assert!(matches!(
        cli.command,
        Commands::DeepResearch {
            action: None,
            question: Some(ref q),
            ..
        } if q == "how do starlings sing"
    ));
}

#[test]
fn cli_rejects_conflicting_or_orphan_model_flags() {
    use clap::Parser;
    assert!(
        Cli::try_parse_from([
            "mur",
            "official",
            "install",
            "agents/orchestrator",
            "--model-policy",
            "cost-first",
            "--model-ref",
            "primary",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "mur",
            "official",
            "install",
            "agents/orchestrator",
            "--fallback",
            "fallback-one",
        ])
        .is_err()
    );
}
