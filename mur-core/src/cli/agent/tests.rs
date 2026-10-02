use super::AgentAction;
use crate::cli::{Cli, Commands};
use clap::Parser;

fn parse_cli_action(argv: &[&str]) -> AgentAction {
    let cli = Cli::try_parse_from(argv).expect("parse argv");
    match cli.command {
        Commands::Agent { action } => action,
        _ => panic!("expected Agent variant"),
    }
}

#[test]
fn agent_cli_accepts_multiple_names() {
    let AgentAction::Cli {
        names,
        resume,
        auto,
        ask: _,
        skin: _,
        plain: _,
        budget_usd: _,
        auto_reads: _,
        no_auto_reads: _,
        fleet: _,
    } = parse_cli_action(&["mur", "agent", "cli", "a1", "a2", "a3", "--auto"])
    else {
        panic!("expected Cli variant");
    };
    assert_eq!(names, vec!["a1", "a2", "a3"]);
    assert!(!resume);
    assert!(auto);
}

#[test]
fn agent_cli_single_name_still_parses() {
    let AgentAction::Cli { names, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"]) else {
        panic!("expected Cli variant");
    };
    assert_eq!(names, vec!["mur"]);
}

#[test]
fn agent_cli_requires_at_least_one_name() {
    assert!(Cli::try_parse_from(["mur", "agent", "cli"]).is_err());
}

/// The read lane is on unless the operator opts out, and `--auto-reads`
/// survives as a no-op so old scripts and muscle memory keep parsing.
/// Pinned at the flag layer because the default lives in `dispatch`'s
/// `!no_auto_reads`, where nothing else would catch an inversion.
#[test]
fn auto_reads_is_the_default_and_the_opt_out_parses() {
    let AgentAction::Cli { no_auto_reads, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"])
    else {
        panic!("expected Cli variant");
    };
    assert!(!no_auto_reads, "the read lane must default to ON");

    let AgentAction::Cli { no_auto_reads, .. } =
        parse_cli_action(&["mur", "agent", "cli", "mur", "--no-auto-reads"])
    else {
        panic!("expected Cli variant");
    };
    assert!(no_auto_reads);

    // Legacy flag: still accepted, now redundant.
    let AgentAction::Cli {
        auto_reads,
        no_auto_reads,
        ..
    } = parse_cli_action(&["mur", "agent", "cli", "mur", "--auto-reads"])
    else {
        panic!("expected Cli variant");
    };
    assert!(auto_reads);
    assert!(!no_auto_reads);

    // Asking for both at once is a contradiction, not a precedence puzzle.
    assert!(
        Cli::try_parse_from([
            "mur",
            "agent",
            "cli",
            "mur",
            "--auto-reads",
            "--no-auto-reads"
        ])
        .is_err()
    );
}

#[test]
fn cli_action_parses_fleet_flag() {
    let AgentAction::Cli { names, fleet, .. } =
        parse_cli_action(&["mur", "agent", "cli", "mur", "--fleet", "develop"])
    else {
        panic!("expected Cli action");
    };
    assert_eq!(names, vec!["mur".to_string()]);
    assert_eq!(fleet.as_deref(), Some("develop"));

    // Absent by default — a plain murmur must not become fleet-aware.
    let AgentAction::Cli { fleet, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"]) else {
        panic!("expected Cli action");
    };
    assert_eq!(fleet, None);
}

#[test]
fn mcp_registry_add_force_flag_parses() {
    let cli = Cli::try_parse_from([
        "mur",
        "agent",
        "mcp",
        "registry-add",
        "rustsmith",
        "com.example/fs",
        "--force",
    ])
    .expect("parse argv");
    let Commands::Agent {
        action: AgentAction::Mcp { action },
    } = cli.command
    else {
        panic!("expected Agent::Mcp variant");
    };
    let crate::cli::agent::AgentMcpAction::RegistryAdd {
        name,
        server,
        force,
    } = action
    else {
        panic!("expected Mcp::RegistryAdd variant");
    };
    assert_eq!(name, "rustsmith");
    assert_eq!(server, "com.example/fs");
    assert!(force);
}

#[test]
fn mcp_registry_add_defaults_force_to_false() {
    let cli = Cli::try_parse_from([
        "mur",
        "agent",
        "mcp",
        "registry-add",
        "rustsmith",
        "com.example/fs",
    ])
    .expect("parse argv");
    let Commands::Agent {
        action: AgentAction::Mcp { action },
    } = cli.command
    else {
        panic!("expected Agent::Mcp variant");
    };
    let crate::cli::agent::AgentMcpAction::RegistryAdd { force, .. } = action else {
        panic!("expected Mcp::RegistryAdd variant");
    };
    assert!(!force);
}

#[test]
fn mcp_add_arg_accepts_hyphen_prefixed_values() {
    // Regression: `--arg --engine` must be consumed as the value, not
    // rejected as an unknown flag (allow_hyphen_values).
    let cli = Cli::try_parse_from([
        "mur",
        "agent",
        "mcp",
        "add",
        "t",
        "x",
        "--command",
        "foo",
        "--arg",
        "--engine",
    ])
    .expect("parse argv");
    let Commands::Agent {
        action: AgentAction::Mcp { action },
    } = cli.command
    else {
        panic!("expected Agent::Mcp variant");
    };
    let crate::cli::agent::AgentMcpAction::Add { args, .. } = action else {
        panic!("expected Mcp::Add variant");
    };
    assert_eq!(args, vec!["--engine".to_string()]);
}
