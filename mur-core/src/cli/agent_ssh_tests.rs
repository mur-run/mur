use super::*;

const FP: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn allow(extra: &[&str]) -> Result<AgentSshAction, clap::Error> {
    let argv = [&["mur", "agent", "ssh", "allow", "bot"], extra].concat();
    let cli = Cli::try_parse_from(argv)?;
    let Commands::Agent {
        action: AgentAction::Ssh { action },
    } = cli.command
    else {
        panic!("not `agent ssh`");
    };
    Ok(action)
}

/// The three key sources of the card, from the CLI (O3).
#[test]
fn allow_takes_a_key_or_generates_one() {
    assert!(allow(&[]).is_err(), "one of --key / --generate");
    assert!(allow(&["--key", FP]).is_ok());
    let AgentSshAction::Allow {
        key,
        generate,
        install,
        user,
        ..
    } = allow(&[
        "--generate",
        "--host",
        "gh.example",
        "--install",
        "--user",
        "git",
    ])
    .unwrap()
    else {
        panic!("not allow");
    };
    assert_eq!(key, None);
    assert!(generate && install);
    assert_eq!(user.as_deref(), Some("git"));
    // The stored key by fingerprint, installed.
    assert!(allow(&["--key", FP, "--host", "h", "--install", "--user", "git"]).is_ok());
}

#[test]
fn allow_refuses_the_flag_combinations_that_skip_a_confirmation() {
    for bad in [
        &["--generate", "--key", FP][..],
        // `--yes` never generates a key or logs in to a host.
        &["--generate", "--yes", "--host-key", FP],
        &[
            "--key",
            FP,
            "--yes",
            "--host-key",
            FP,
            "--install",
            "--user",
            "git",
        ],
        // `--install` needs both where and as whom.
        &["--generate", "--install", "--user", "git"],
        &["--generate", "--install", "--host", "h"],
        &["--generate", "--host", "h", "--user", "git"],
    ] {
        assert!(allow(bad).is_err(), "{bad:?}");
    }
}
